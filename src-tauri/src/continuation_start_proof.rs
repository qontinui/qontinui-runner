//! The runner half of coord's continuation START PROOF (plan
//! `2026-11-10-coord-counts-a-gate-continuation-consumed-only-once-its-session-proves-it-started`,
//! D1, D4 and Phase 2).
//!
//! Coord used to count a gate continuation consumed the moment the runner
//! CLAIMED it, before any `claude` had started. On 2026-11-10 a migrated
//! continuation pane died with exit 127 nine seconds after spawn (`claude` was
//! off PATH mid-reinstall) and coord had already marked its gate consumed, so
//! nothing ever re-drove it. The fix is for coord to decide "started" from an
//! event only the launched CLI can cause, and this module produces that event.
//!
//! Two posts, both best-effort, both off the caller's path:
//!
//! - **`continuation-started`** — `POST /coord/agent-gates/{gate_id}/continuation-started`
//!   `{attempt, claude_session_id, terminal_id}`, fired when a continuation
//!   pane's `SessionStart` hook reaches the runner's `GET /sessions/{id}/policy-context`
//!   route ([`on_session_start`]). That route is hit by every `SessionStart`
//!   whatever `QONTINUI_POLICY_INJECTION` says, which is why the hook sits
//!   before the route's mode branch rather than at the "injecting" log line
//!   (D1). Coord accepts it only when `claude_session_id` equals the session id
//!   it bound onto the gate's placeholder row, so the runner relays a proof it
//!   cannot originate. `terminal_id` is the pane whose `SessionStart` produced
//!   the proof (the route's raw path id, also for a proof parked before its
//!   registration); while a respawn terminal is stored, coord refuses a proof
//!   from any other terminal (`409 stale_terminal`, a quiet decline).
//! - **`continuation-respawned`** — `POST /coord/agent-gates/{gate_id}/continuation-respawned`
//!   `{attempt, new_terminal_id}`, fired when a carried continuation is
//!   re-pinned onto a NEW pane ([`report_respawned`]) by the account
//!   migration (D4). `attempt` is the attempt the continuation was CLAIMED
//!   under, unchanged by the hop; `new_terminal_id` is the NEW pane's terminal
//!   id. Coord keys the reset's idempotency on `new_terminal_id`, so a report
//!   naming a terminal it has not seen for this gate clears the standing proof
//!   even at the same attempt, while a repeat of the same report is a no-op.
//!   The new pane then proves its own start with a `continuation-started`
//!   carrying that same attempt and the Claude session id its own
//!   `SessionStart` hook reports.
//!
//! The resume door is deliberately NOT a reporter: it reopens the SAME Claude
//! session on a pane that registers no continuation, so nothing on that pane
//! could ever re-prove a reset proof, and the proof the session already earned
//! stays valid.
//!
//! Both posts retry a transient answer in-task ([`post_with_retry`]): no
//! answer, any 5xx, and `schema_migration_pending`, at most
//! [`RETRY_DELAYS`]`.len() + 1` tries. A 4xx is never retried.
//!
//! ## The two races this module closes
//!
//! **SessionStart can arrive before the registration.** The dispatch path
//! registers a continuation terminal only after its start verdict (the pinned
//! transcript exists, or the PTY outlived the start window), and the account
//! migration re-pins only after its synchronous respawn returns. A `claude`
//! that starts quickly runs its `SessionStart` hook BEFORE either. So a hook
//! that finds no registration is not dropped: it is parked in
//! [`ProofState::pending`] for [`PENDING_TTL`], and the registration itself
//! ([`on_continuation_registered`], called from the registry's single insert
//! point) consumes it. Both sides REMOVE the parked entry, so exactly one of
//! them emits whichever order they run in:
//!
//! - the hook parks first, then reads the registry; and
//! - the registration takes the parked entry and inserts the live row, both
//!   inside one registry critical section (`insert_live` takes, then inserts).
//!
//! What makes that exhaustive is the continuation REGISTRY's mutex, not the
//! order of the two statements inside the registration: the take runs inside
//! the same registry critical section as the insert, and the hook's read takes
//! that same lock. The hook's read is therefore wholly before that critical
//! section (the hook parked earlier still, so the take finds the entry) or
//! wholly after it (the read sees the row). There is no interleaving in which
//! both miss.
//!
//! ## Lock order
//!
//! Three locks meet here: the continuation registry's, this module's own
//! [`ProofState`] lock, and the transport's `sender_slot` mutex. The only
//! nestings are registry → proof state (the registration's take runs under the
//! registry lock) and registry → `sender_slot` (the registration's dispatch
//! runs [`current_sender`] under it). This module never takes the registry
//! lock while it holds its own: the hook parks, RELEASES, then reads the
//! registry, then takes its own lock again. `sender_slot` is a LEAF: it is
//! taken only by [`current_sender`] (and the test setter), which clones the
//! sender and releases it with no other lock taken in between, and it is never
//! taken while the [`ProofState`] lock is held. So none of the three can
//! deadlock.
//!
//! ## Stale retries are fenced
//!
//! Every respawn bumps a per-gate generation ([`ProofState::gate_generation`]).
//! Each post captures its gate's generation when it is planned — a start proof
//! in [`plan_started`], under the same lock as its dedupe mark; a reset in
//! [`plan_respawned`], which is what bumps it — and [`post_with_retry`] checks
//! it before EVERY try and once more after the last: a post whose gate has
//! been respawned since is dropped (not sent, or its answer discarded). Otherwise the old pane's start proof,
//! asleep in a 5xx backoff across the hop, would land after the reset and
//! re-prove a pane that is gone, and an older respawn's retry would re-clear
//! the new pane's proof. A dropped start proof also skips its dedupe un-mark,
//! which would otherwise clear the new pane's key (same gate, attempt and —
//! after a migration — Claude session).
//!
//! **A respawn's reset should reach coord before the new pane's proof.** The
//! two posts are independent tasks, so nothing orders them on the wire by
//! itself. [`report_respawned`] therefore opens a per-gate barrier
//! SYNCHRONOUSLY, before the caller re-pins the registration, and every start
//! proof for that gate awaits the barrier before it posts. The new pane's
//! proof can only be planned once the re-pin has run, and the re-pin runs after
//! the barrier opened, so the start proof is sent only after the reset was
//! answered or its retries were exhausted. The wait is bounded by
//! [`RESPAWN_BARRIER_WAIT`], longer than the reset's whole retry budget; past
//! it the proof is posted anyway, and coord sees whichever arrives. The respawn also forgets the gate's dedupe
//! entries: a migration resumes the SAME Claude session id, and without that
//! the new pane's proof would be dropped as a duplicate of the old pane's.
//!
//! ## What `attempt` is, and when it is UNKNOWN
//!
//! `attempt` is coord's own re-delivery episode — the attempt the runner
//! CLAIMED the continuation under (`ConsumeTarget::Gate`'s second field:
//! `payload.retry.attempt`, or for a poll row the max of that and coord's
//! `continuation_spawn_attempts`). It is recorded per gate when the claim wins
//! ([`record_dispatch_attempt`]) and carried by gate id across a migration.
//!
//! It is process-local. A continuation re-adopted after a runner restart (the
//! boot-restore path, `register_restored_continuation`) or reopened by the
//! resume door carries a gate id but no attempt this process ever saw, and the
//! durable lifecycle record does not store one. Inventing `0` there would be a
//! fabricated value coord stores as fact, so such a proof is NOT posted: it is
//! logged at `info` naming the gate, and coord reads the start as unproven
//! (served policy `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`).
//!
//! ## Dedupe
//!
//! At most one start proof per `(gate_id, attempt, claude_session_id,
//! terminal_id)` per runner process. `SessionStart` also fires on `--resume`
//! and on `compact`, so without it one long session would post on every
//! compaction. A resume of the
//! same session is a legitimate re-proof to coord, which is why the dedupe is
//! in-process only and why a respawn clears it. A proof whose retries end on a
//! transient answer un-marks its key, so the next `SessionStart` of that
//! session tries again.
//!
//! ## Credential
//!
//! The same shared coord client and device-JWT seam every other runner→coord
//! continuation call uses ([`crate::coord_http::coord_client`],
//! [`crate::auth::attach_device_auth_for`]). The gate row HAS an owning tenant
//! and the runner holds no record of which one, so it states
//! [`crate::auth::TenantScope::Unresolved`]: the default binding's credential
//! on a single-bound device, and no bearer (an honest refusal) on a
//! multi-bound one, never a guessed tenant's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use uuid::Uuid;

/// How long a `SessionStart` that found no registration waits for one. The
/// dispatch path's start window is 30 s per attempt with up to three attempts;
/// a migration's respawn is seconds. Ten minutes covers both with room, and is
/// the order of coord's own start-proof deadline (600 s).
const PENDING_TTL: Duration = Duration::from_secs(600);

/// Bound on parked `SessionStart`s. Every operator tab's `SessionStart` parks
/// briefly too (nothing can tell it apart from an early continuation until the
/// registration does or does not come), so the map is bounded as well as aged.
const PENDING_CAP: usize = 512;

/// Bound on the per-gate attempt memory and on the dedupe set — each entry is
/// a few dozen bytes, but a runner lives for weeks.
const MEMORY_CAP: usize = 4096;

/// Per-request bound on the start-proof POST.
const START_POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-request bound on the respawn POST — short, because it is on the
/// migration's account-hop path in spirit even though it never blocks it.
const RESPAWN_POST_TIMEOUT: Duration = Duration::from_secs(5);

/// Backoff between tries of one post: three tries over about a minute. Only a
/// transient answer is retried ([`PostVerdict::is_transient`]).
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(15), Duration::from_secs(40)];

/// How long a start proof waits for an in-flight respawn reset of the same
/// gate before posting anyway. Longer than the reset's whole retry budget —
/// every try's timeout plus every backoff — so a reset that gives up has
/// already settled.
const RESPAWN_BARRIER_WAIT: Duration = Duration::from_secs(80);

/// One post this module makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProofPost {
    /// `continuation-started {attempt, claude_session_id, terminal_id}`.
    /// `terminal_id` is the pane whose `SessionStart` produced the proof — for
    /// a migrated pane the NEW pane, the same id its reset sent as
    /// `new_terminal_id`.
    Started {
        gate_id: Uuid,
        attempt: u32,
        claude_session_id: Uuid,
        terminal_id: String,
    },
    /// `continuation-respawned {attempt, new_terminal_id}`.
    Respawned {
        gate_id: Uuid,
        attempt: u32,
        new_terminal_id: String,
    },
}

impl ProofPost {
    fn gate_id(&self) -> Uuid {
        match self {
            ProofPost::Started { gate_id, .. } | ProofPost::Respawned { gate_id, .. } => *gate_id,
        }
    }

    /// The dedupe key of a start proof; `None` for a reset.
    fn proof_key(&self) -> Option<ProofKey> {
        match self {
            ProofPost::Started {
                gate_id,
                attempt,
                claude_session_id,
                terminal_id,
            } => Some((*gate_id, *attempt, *claude_session_id, terminal_id.clone())),
            ProofPost::Respawned { .. } => None,
        }
    }

    /// The route suffix under `/coord/agent-gates/{gate_id}/`.
    fn verb(&self) -> &'static str {
        match self {
            ProofPost::Started { .. } => "continuation-started",
            ProofPost::Respawned { .. } => "continuation-respawned",
        }
    }

    /// The path under the coord base.
    pub(crate) fn path(&self) -> String {
        format!("/coord/agent-gates/{}/{}", self.gate_id(), self.verb())
    }

    /// The wire body — exactly the field names coord's Phase 1 receiver reads.
    pub(crate) fn body(&self) -> serde_json::Value {
        match self {
            ProofPost::Started {
                attempt,
                claude_session_id,
                terminal_id,
                ..
            } => serde_json::json!({
                "attempt": attempt,
                "claude_session_id": claude_session_id.to_string(),
                "terminal_id": terminal_id,
            }),
            ProofPost::Respawned {
                attempt,
                new_terminal_id,
                ..
            } => serde_json::json!({
                "attempt": attempt,
                "new_terminal_id": new_terminal_id,
            }),
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            ProofPost::Started { .. } => START_POST_TIMEOUT,
            ProofPost::Respawned { .. } => RESPAWN_POST_TIMEOUT,
        }
    }
}

/// What coord's answer to one post means. Classified from the status and
/// body alone ([`classify_response`]) so the reading is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PostVerdict {
    /// 2xx with `recorded` true (or absent) — coord recorded it.
    Accepted,
    /// 2xx with `"recorded": false` — coord's start-proof switch is `off`, so
    /// it acknowledged without recording. Not an error.
    NotRecorded,
    /// 404 — a coord predating the Phase 1 receiver, or `gate_not_found`. Not
    /// an error: the start stays unproven, exactly as before.
    RouteAbsent,
    /// 503 `schema_migration_pending` — coord's receiver is deployed but its
    /// `coord.gates` start-proof columns are not migrated yet.
    SchemaPending,
    /// 409 `stale_attempt` / `continuation_not_consumed` / `no_placeholder` /
    /// `continuation_retired` / `stale_terminal` — an ordinary race (a replaced
    /// pane's late report, a gate never consumed, a placeholder already gone, a
    /// continuation coord has retired, a proof from a terminal other than the
    /// stored respawn terminal). Quiet: nothing this runner can fix.
    Declined { status: u16, code: String },
    /// 409 `session_mismatch` / `foreign_session`, 403 `not_consumer`, or any
    /// answer this build does not recognise — coord refused a proof the runner
    /// believed in, which means the runner's view of the gate is wrong.
    Rejected { status: u16, detail: String },
    /// No answer: no coord configured, no client, a transport error or the
    /// timeout. Transient, like a 5xx and `SchemaPending`: a start proof whose
    /// retries END on any transient verdict un-marks its dedupe key.
    NoAnswer(String),
}

impl PostVerdict {
    /// Worth another try: no answer, any 5xx (a `500 db_error`, every 503),
    /// and `schema_migration_pending`. Never a 4xx, never a 2xx.
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            PostVerdict::NoAnswer(_) | PostVerdict::SchemaPending => true,
            PostVerdict::Rejected { status, .. } => *status >= 500,
            PostVerdict::Accepted
            | PostVerdict::NotRecorded
            | PostVerdict::RouteAbsent
            | PostVerdict::Declined { .. } => false,
        }
    }
}

/// Whether the gate's respawn generation still equals `generation` — i.e. no
/// respawn has been reported since the post holding it was planned.
fn still_current(gate_id: Uuid, generation: u64) -> bool {
    state().generation(gate_id) == generation
}

/// Post `post`, retrying a transient answer after each of [`RETRY_DELAYS`].
/// `generation` is the gate's respawn generation when `post` was planned;
/// before EVERY try the post is dropped if it has moved. Returns the last
/// verdict (`None` when the post was dropped as superseded) and how many tries
/// were sent.
async fn post_with_retry(
    sender: &dyn ProofSender,
    post: &ProofPost,
    generation: u64,
) -> (Option<PostVerdict>, usize) {
    let gate_id = post.gate_id();
    if !still_current(gate_id, generation) {
        return (None, 0);
    }
    let mut tries = 1;
    let mut verdict = sender.post(post.clone()).await;
    for delay in RETRY_DELAYS {
        if !verdict.is_transient() {
            break;
        }
        debug!(
            gate_id = %post.gate_id(),
            "{}: transient answer {verdict:?} — retrying in {}s",
            post.verb(),
            delay.as_secs()
        );
        tokio::time::sleep(delay).await;
        if !still_current(gate_id, generation) {
            debug!(
                gate_id = %gate_id,
                "{}: gate respawned during the backoff — dropping the stale retry",
                post.verb()
            );
            return (None, tries);
        }
        tries += 1;
        verdict = sender.post(post.clone()).await;
    }
    // Once more after the LAST try: a respawn reported while that try was in
    // flight supersedes its answer too, so the caller neither un-marks a key
    // that may now be the new pane's nor reports an old reset as FAILED.
    if !still_current(gate_id, generation) {
        debug!(
            gate_id = %gate_id,
            "{}: gate respawned while the last try was in flight — discarding its answer \
             {verdict:?}",
            post.verb()
        );
        return (None, tries);
    }
    (Some(verdict), tries)
}

/// The `error` code of a coord JSON error body, if it has one.
fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
}

/// Read coord's answer — the receiver's contract (qontinui-coord f75b9a97a):
/// `200 {"accepted": true, "recorded": bool, ..}`, or `{"error": <code>, ..}`.
pub(crate) fn classify_response(status: u16, body: &str) -> PostVerdict {
    if (200..300).contains(&status) {
        let recorded = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("recorded").and_then(serde_json::Value::as_bool));
        return if recorded == Some(false) {
            PostVerdict::NotRecorded
        } else {
            PostVerdict::Accepted
        };
    }
    let code = error_code(body);
    match (status, code.as_deref()) {
        (404, _) => PostVerdict::RouteAbsent,
        (503, Some("schema_migration_pending")) => PostVerdict::SchemaPending,
        (
            409,
            Some(
                c @ ("stale_attempt"
                | "continuation_not_consumed"
                | "no_placeholder"
                | "continuation_retired"
                | "stale_terminal"),
            ),
        ) => PostVerdict::Declined {
            status,
            code: c.to_string(),
        },
        _ => PostVerdict::Rejected {
            status,
            detail: body.chars().take(200).collect(),
        },
    }
}

/// The transport seam. Production is [`HttpProofSender`]; tests install a
/// recorder through [`set_sender_for_test`].
pub(crate) trait ProofSender: Send + Sync {
    fn post(&self, post: ProofPost) -> Pin<Box<dyn Future<Output = PostVerdict> + Send>>;
}

/// The production sender: the shared coord client, device JWT, bounded.
struct HttpProofSender;

impl ProofSender for HttpProofSender {
    fn post(&self, post: ProofPost) -> Pin<Box<dyn Future<Output = PostVerdict> + Send>> {
        Box::pin(async move {
            let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() else {
                return PostVerdict::NoAnswer("no coord base configured".to_string());
            };
            let Some(client) = crate::coord_http::coord_client() else {
                return PostVerdict::NoAnswer("no shared coord client".to_string());
            };
            let url = format!("{base}{}", post.path());
            let sent = crate::auth::attach_device_auth_for(
                client.post(&url),
                crate::auth::TenantScope::Unresolved,
            )
            .timeout(post.timeout())
            .json(&post.body())
            .send()
            .await;
            match sent {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    classify_response(status, &text)
                }
                Err(e) => PostVerdict::NoAnswer(format!("{e:#}")),
            }
        })
    }
}

fn sender_slot() -> &'static Mutex<Arc<dyn ProofSender>> {
    static SENDER: OnceLock<Mutex<Arc<dyn ProofSender>>> = OnceLock::new();
    SENDER.get_or_init(|| Mutex::new(Arc::new(HttpProofSender)))
}

fn current_sender() -> Arc<dyn ProofSender> {
    sender_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Replace the transport (tests only).
#[cfg(test)]
pub(crate) fn set_sender_for_test(sender: Arc<dyn ProofSender>) {
    *sender_slot().lock().unwrap_or_else(|p| p.into_inner()) = sender;
}

/// A `SessionStart` that found no continuation registration yet.
#[derive(Debug, Clone, Copy)]
struct ParkedStart {
    claude_session_id: Uuid,
    at: Instant,
}

/// Dedupe key: one proof per `(gate, attempt, session, terminal)` per process.
type ProofKey = (Uuid, u32, Uuid, String);

/// The module's process state, under ONE lock. Never held while the
/// continuation registry's lock is taken (see the module doc's "Lock order").
#[derive(Default)]
struct ProofState {
    /// gate id → the attempt this process claimed it under.
    attempts: HashMap<Uuid, u32>,
    attempt_order: VecDeque<Uuid>,
    /// terminal id → a `SessionStart` waiting for its registration.
    pending: HashMap<String, ParkedStart>,
    /// Keys already posted or in flight.
    sent: HashSet<ProofKey>,
    sent_order: VecDeque<ProofKey>,
    /// gate id → the barrier of a respawn reset still in flight, tagged with
    /// the generation that opened it (a second respawn of the same gate
    /// replaces it, and only the opener may remove its own).
    respawn_inflight: HashMap<Uuid, (u64, tokio::sync::watch::Receiver<bool>)>,
    /// gate id → how many respawns this process has reported for it. A post
    /// captures it when planned and is dropped once it moves (see the module
    /// doc's "Stale retries are fenced"). Absent reads 0. Its keys are a
    /// subset of [`Self::attempts`]' and are evicted with them.
    gate_generation: HashMap<Uuid, u64>,
}

impl ProofState {
    fn remember_attempt(&mut self, gate_id: Uuid, attempt: u32) {
        if self.attempts.insert(gate_id, attempt).is_none() {
            self.attempt_order.push_back(gate_id);
            while self.attempt_order.len() > MEMORY_CAP {
                if let Some(old) = self.attempt_order.pop_front() {
                    self.attempts.remove(&old);
                    self.gate_generation.remove(&old);
                }
            }
        }
    }

    fn park(&mut self, terminal_id: &str, claude_session_id: Uuid, now: Instant) {
        self.pending
            .retain(|_, p| now.saturating_duration_since(p.at) < PENDING_TTL);
        if self.pending.len() >= PENDING_CAP && !self.pending.contains_key(terminal_id) {
            // Drop the oldest: a stale operator tab, by far the likeliest.
            if let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, p)| p.at)
                .map(|(k, _)| k.clone())
            {
                self.pending.remove(&oldest);
            }
        }
        self.pending.insert(
            terminal_id.to_string(),
            ParkedStart {
                claude_session_id,
                at: now,
            },
        );
    }

    fn take_parked(&mut self, terminal_id: &str, now: Instant) -> Option<Uuid> {
        let parked = self.pending.remove(terminal_id)?;
        (now.saturating_duration_since(parked.at) < PENDING_TTL).then_some(parked.claude_session_id)
    }

    /// Mark `key` in flight; `false` when it already was (a duplicate).
    fn mark(&mut self, key: ProofKey) -> bool {
        if !self.sent.insert(key.clone()) {
            return false;
        }
        self.sent_order.push_back(key);
        while self.sent_order.len() > MEMORY_CAP {
            if let Some(old) = self.sent_order.pop_front() {
                self.sent.remove(&old);
            }
        }
        true
    }

    fn unmark(&mut self, key: &ProofKey) {
        self.sent.remove(key);
        self.sent_order.retain(|k| k != key);
    }

    fn generation(&self, gate_id: Uuid) -> u64 {
        self.gate_generation.get(&gate_id).copied().unwrap_or(0)
    }

    fn forget_gate(&mut self, gate_id: Uuid) {
        self.sent.retain(|(g, ..)| *g != gate_id);
        self.sent_order.retain(|(g, ..)| *g != gate_id);
    }
}

fn state() -> MutexGuard<'static, ProofState> {
    static STATE: OnceLock<Mutex<ProofState>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(ProofState::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Reset all process state (tests only).
#[cfg(test)]
pub(crate) fn reset_for_test() {
    *state() = ProofState::default();
}

/// Record the attempt a gate continuation was CLAIMED under. Called by the
/// dispatcher when coord's consume claim lets the spawn proceed.
pub(crate) fn record_dispatch_attempt(gate_id: Uuid, attempt: u32) {
    state().remember_attempt(gate_id, attempt);
}

/// A start proof ready to dispatch, with the gate's respawn generation captured
/// when it was planned (under the same lock as its dedupe mark).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedStart {
    post: ProofPost,
    generation: u64,
}

/// Decide the start proof for a continuation terminal whose session is known:
/// `None` for an unknown attempt (logged) or a duplicate.
fn plan_started(terminal_id: &str, gate_id: Uuid, claude_session_id: Uuid) -> Option<PlannedStart> {
    let mut st = state();
    let Some(attempt) = st.attempts.get(&gate_id).copied() else {
        drop(st);
        info!(
            gate_id = %gate_id,
            terminal_id,
            claude_session_id = %claude_session_id,
            "continuation start proof NOT posted: this process holds no claimed attempt for \
             the gate (a continuation restored or reopened from a previous runner process) — \
             coord reads its start as unproven"
        );
        return None;
    };
    if !st.mark((gate_id, attempt, claude_session_id, terminal_id.to_string())) {
        drop(st);
        debug!(
            gate_id = %gate_id,
            attempt,
            claude_session_id = %claude_session_id,
            "continuation start proof already posted this process — not re-posting"
        );
        return None;
    }
    Some(PlannedStart {
        post: ProofPost::Started {
            gate_id,
            attempt,
            claude_session_id,
            terminal_id: terminal_id.to_string(),
        },
        generation: st.generation(gate_id),
    })
}

/// The `SessionStart` half: the hook for terminal `terminal_id` reported
/// Claude session `claude_session_id`. Returns the proof to post, if any —
/// `gate_for` is the continuation registry's terminal→gate lookup.
fn plan_session_start(
    terminal_id: &str,
    claude_session_id: Uuid,
    gate_for: &dyn Fn(&str) -> Option<Uuid>,
) -> Option<PlannedStart> {
    let now = Instant::now();
    // Park FIRST, then read the registry — see the module doc for why this
    // order (with the registration's insert-then-take) can never lose both.
    state().park(terminal_id, claude_session_id, now);
    let gate_id = gate_for(terminal_id)?;
    // Registered: whichever side takes the parked entry emits. If the
    // registration already took it, it has emitted and this is a no-op.
    let parked = state().take_parked(terminal_id, now)?;
    plan_started(terminal_id, gate_id, parked)
}

/// The registration half: `terminal_id` was just inserted into the live
/// continuation registry for `gate_id`. Returns the proof for a parked
/// `SessionStart`, if one is waiting.
fn plan_registered(terminal_id: &str, gate_id: Option<Uuid>) -> Option<PlannedStart> {
    let parked = state().take_parked(terminal_id, Instant::now())?;
    let gate_id = gate_id?;
    plan_started(terminal_id, gate_id, parked)
}

/// Called by `GET /sessions/{id}/policy-context` for EVERY `SessionStart`,
/// before the route's injection-mode branch. `terminal_id` is the route's raw
/// path id (the hook's `QONTINUI_TERMINAL_ID`); `claude_session_id` is the
/// hook payload's session id, already strictly parsed. Never blocks: the post
/// runs on a spawned task.
pub(crate) fn on_session_start(terminal_id: &str, claude_session_id: Option<Uuid>) {
    let Some(claude_session_id) = claude_session_id else {
        // A hook that forwards no session id cannot prove anything. Not parked:
        // there is nothing to join on.
        return;
    };
    let lookup = |tid: &str| crate::agent_runtime::continuation_gate_for_terminal(tid);
    if let Some(planned) = plan_session_start(terminal_id, claude_session_id, &lookup) {
        dispatch(planned);
    }
}

/// Called from the continuation registry's single insert point. May run
/// under the registry's lock: it takes only this module's lock and spawns.
pub(crate) fn on_continuation_registered(terminal_id: &str, gate_id: Option<Uuid>) {
    if let Some(planned) = plan_registered(terminal_id, gate_id) {
        dispatch(planned);
    }
}

/// Plan the respawn reset for `gate_id` onto `new_terminal_id`: forget the
/// gate's dedupe entries, and open the barrier start proofs wait on. Returns
/// the post and the barrier's sender, or `None` (logged) with an unknown
/// attempt.
fn plan_respawned(
    gate_id: Uuid,
    new_terminal_id: &str,
) -> Option<(ProofPost, u64, tokio::sync::watch::Sender<bool>)> {
    let mut st = state();
    // Forget dedupe either way: the new pane must prove its own start.
    st.forget_gate(gate_id);
    let Some(attempt) = st.attempts.get(&gate_id).copied() else {
        drop(st);
        info!(
            gate_id = %gate_id,
            new_terminal_id,
            "continuation-respawned NOT posted: this process holds no claimed attempt for the \
             gate (a continuation restored or reopened from a previous runner process)"
        );
        return None;
    };
    let (tx, rx) = tokio::sync::watch::channel(false);
    let generation = st.generation(gate_id) + 1;
    st.gate_generation.insert(gate_id, generation);
    st.respawn_inflight.insert(gate_id, (generation, rx));
    Some((
        ProofPost::Respawned {
            gate_id,
            attempt,
            new_terminal_id: new_terminal_id.to_string(),
        },
        generation,
        tx,
    ))
}

/// Report that the carried continuation for `gate_id` is being re-pinned onto
/// the NEW pane `new_terminal_id` (D4). Call it before the re-pin
/// (`restore_continuation_registration`) — that is what orders this reset
/// ahead of the new pane's own start proof. Infallible and non-blocking: the
/// post runs on a spawned task, so it can sit inside the migration's
/// anchor-held window. `None` gate (a work-unit dispatch) does nothing.
pub(crate) fn report_respawned(gate_id: Option<Uuid>, new_terminal_id: &str) {
    let Some(gate_id) = gate_id else {
        return;
    };
    let Some((post, generation, barrier)) = plan_respawned(gate_id, new_terminal_id) else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        debug!(gate_id = %gate_id, "continuation-respawned: no tokio runtime — not posted");
        state().respawn_inflight.remove(&gate_id);
        return;
    };
    let sender = current_sender();
    handle.spawn(async move {
        let (verdict, tries) = post_with_retry(sender.as_ref(), &post, generation).await;
        if let Some(verdict) = &verdict {
            log_verdict(&post, verdict);
        }
        if verdict.as_ref().is_some_and(PostVerdict::is_transient) {
            warn!(
                gate_id = %gate_id,
                tries,
                "continuation-respawned for gate {gate_id} FAILED after {tries} tries — coord \
                 still holds the old pane's start proof for it"
            );
        }
        // Release the barrier, then drop it from the map — but only if it is
        // still OURS (a second respawn may have replaced it).
        let _ = barrier.send(true);
        let mut st = state();
        if st
            .respawn_inflight
            .get(&gate_id)
            .is_some_and(|(g, _)| *g == generation)
        {
            st.respawn_inflight.remove(&gate_id);
        }
    });
}

/// Post `post` on a spawned task. A start proof first waits (bounded) for any
/// in-flight respawn reset of its gate.
fn dispatch(planned: PlannedStart) {
    let PlannedStart { post, generation } = planned;
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        debug!(gate_id = %post.gate_id(), "continuation start proof: no tokio runtime — not posted");
        if let Some(key) = post.proof_key() {
            state().unmark(&key);
        }
        return;
    };
    let barrier = state()
        .respawn_inflight
        .get(&post.gate_id())
        .map(|(_, rx)| rx.clone());
    let sender = current_sender();
    handle.spawn(async move {
        if let Some(mut rx) = barrier {
            if tokio::time::timeout(RESPAWN_BARRIER_WAIT, rx.wait_for(|done| *done))
                .await
                .is_err()
            {
                debug!(
                    gate_id = %post.gate_id(),
                    "continuation start proof: respawn reset still unanswered — posting anyway"
                );
            }
        }
        let (verdict, _tries) = post_with_retry(sender.as_ref(), &post, generation).await;
        // Dropped as superseded: no log of a verdict that never came, and NO
        // un-mark — the key may now be the new pane's.
        let Some(verdict) = verdict else {
            return;
        };
        log_verdict(&post, &verdict);
        if let Some(key) = post.proof_key().filter(|_| verdict.is_transient()) {
            // Un-mark so the session's next SessionStart retries.
            state().unmark(&key);
        }
    });
}

fn log_verdict(post: &ProofPost, verdict: &PostVerdict) {
    let verb = post.verb();
    let gate_id = post.gate_id();
    match verdict {
        PostVerdict::Accepted => info!(gate_id = %gate_id, body = %post.body(), "{verb} posted"),
        PostVerdict::RouteAbsent => debug!(
            gate_id = %gate_id,
            "{verb}: coord answered 404 (a coord predating the start-proof receiver) — continuing"
        ),
        PostVerdict::SchemaPending => debug!(
            gate_id = %gate_id,
            "{verb}: coord answered schema_migration_pending — continuing"
        ),
        PostVerdict::NotRecorded => debug!(
            gate_id = %gate_id,
            "{verb}: coord acknowledged without recording (its start-proof switch is off)"
        ),
        PostVerdict::Declined { status, code } => info!(
            gate_id = %gate_id,
            status,
            code = %code,
            body = %post.body(),
            "{verb} declined by coord (an ordinary race) — continuing"
        ),
        PostVerdict::Rejected { status, detail } if *status >= 500 => warn!(
            gate_id = %gate_id,
            status,
            detail = %detail,
            body = %post.body(),
            "{verb}: coord failed (5xx)"
        ),
        PostVerdict::Rejected { status, detail } => warn!(
            gate_id = %gate_id,
            status,
            detail = %detail,
            body = %post.body(),
            "{verb} REJECTED by coord — the runner's view of this gate disagrees with coord's"
        ),
        PostVerdict::NoAnswer(why) => warn!(
            gate_id = %gate_id,
            body = %post.body(),
            "{verb} not delivered (continuing): {why}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Serializes this module's tests (shared process state). Tests that also
    /// touch the continuation registry take the registry's lock first.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn gate() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn wire_paths_and_bodies_are_exactly_the_receiver_contract() {
        let g = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let s = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let started = ProofPost::Started {
            gate_id: g,
            attempt: 2,
            claude_session_id: s,
            terminal_id: "term-pane".into(),
        };
        assert_eq!(
            started.path(),
            "/coord/agent-gates/11111111-2222-3333-4444-555555555555/continuation-started"
        );
        assert_eq!(
            started.body(),
            serde_json::json!({
                "attempt": 2,
                "claude_session_id": s.to_string(),
                "terminal_id": "term-pane",
            })
        );
        let respawned = ProofPost::Respawned {
            gate_id: g,
            attempt: 0,
            new_terminal_id: "term-new".into(),
        };
        assert_eq!(
            respawned.path(),
            "/coord/agent-gates/11111111-2222-3333-4444-555555555555/continuation-respawned"
        );
        assert_eq!(
            respawned.body(),
            serde_json::json!({"attempt": 0, "new_terminal_id": "term-new"})
        );
    }

    #[test]
    fn coord_answers_are_classified() {
        assert_eq!(classify_response(200, ""), PostVerdict::Accepted);
        assert_eq!(classify_response(204, ""), PostVerdict::Accepted);
        assert_eq!(classify_response(404, "nope"), PostVerdict::RouteAbsent);
        assert_eq!(
            classify_response(
                503,
                r#"{"error":"schema_migration_pending","missing":"coord.gates.continuation_start_proven_at","cause":"never_seen"}"#
            ),
            PostVerdict::SchemaPending
        );
        assert_eq!(
            classify_response(200, r#"{"accepted":true,"recorded":false,"mode":"off"}"#),
            PostVerdict::NotRecorded
        );
        assert_eq!(
            classify_response(200, r#"{"accepted":true,"recorded":true}"#),
            PostVerdict::Accepted
        );
        assert_eq!(
            classify_response(404, r#"{"error":"gate_not_found"}"#),
            PostVerdict::RouteAbsent
        );
        for code in [
            "stale_attempt",
            "continuation_not_consumed",
            "no_placeholder",
            "continuation_retired",
            "stale_terminal",
        ] {
            assert_eq!(
                classify_response(409, &format!(r#"{{"error":"{code}","message":"m"}}"#)),
                PostVerdict::Declined {
                    status: 409,
                    code: code.to_string()
                },
                "{code}"
            );
        }
        for (status, code) in [
            (409, "session_mismatch"),
            (409, "foreign_session"),
            (403, "not_consumer"),
        ] {
            assert!(
                matches!(
                    classify_response(status, &format!(r#"{{"error":"{code}"}}"#)),
                    PostVerdict::Rejected { .. }
                ),
                "{code}"
            );
        }
        assert!(matches!(
            classify_response(503, r#"{"error":"overloaded"}"#),
            PostVerdict::Rejected { status: 503, .. }
        ));
    }

    /// A registered continuation terminal's SessionStart plans exactly one
    /// proof carrying the claimed attempt and the hook's session id.
    #[test]
    fn a_registered_continuation_session_start_plans_the_proof() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 3);
        let lookup = move |tid: &str| (tid == "term-a").then_some(g);
        assert_eq!(
            plan_session_start("term-a", s, &lookup).map(|p| p.post),
            Some(ProofPost::Started {
                gate_id: g,
                attempt: 3,
                claude_session_id: s,
                terminal_id: "term-a".into(),
            })
        );
    }

    #[test]
    fn a_non_continuation_terminal_plans_nothing() {
        let _l = lock();
        reset_for_test();
        let lookup = |_: &str| None;
        assert_eq!(
            plan_session_start("operator-tab", Uuid::new_v4(), &lookup),
            None
        );
        // …and a later registration of some OTHER terminal does not pick it up.
        assert_eq!(plan_registered("other", Some(gate())), None);
    }

    /// `SessionStart` fires on startup, resume and every compact: the same
    /// (gate, attempt, session) is proven once per process.
    #[test]
    fn repeated_session_starts_dedupe() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        let lookup = move |_: &str| Some(g);
        assert!(plan_session_start("term-d", s, &lookup).is_some());
        assert_eq!(plan_session_start("term-d", s, &lookup), None);
        assert_eq!(plan_session_start("term-d", s, &lookup), None);
        // A new attempt of the same gate is a new proof.
        record_dispatch_attempt(g, 1);
        assert!(plan_session_start("term-d", s, &lookup).is_some());
    }

    /// A transport failure un-marks the key so the next SessionStart retries.
    #[test]
    fn an_undelivered_proof_is_retried_on_the_next_session_start() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        let lookup = move |_: &str| Some(g);
        assert!(plan_session_start("term-r", s, &lookup).is_some());
        state().unmark(&(g, 0, s, "term-r".to_string()));
        assert!(plan_session_start("term-r", s, &lookup).is_some());
    }

    /// The early-SessionStart race: the hook arrives before the registration,
    /// is parked, and the registration emits it — once.
    #[test]
    fn a_session_start_before_registration_is_emitted_by_the_registration() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 1);
        let unregistered = |_: &str| None;
        assert_eq!(plan_session_start("term-early", s, &unregistered), None);
        assert_eq!(
            plan_registered("term-early", Some(g)).map(|p| p.post),
            Some(ProofPost::Started {
                gate_id: g,
                attempt: 1,
                claude_session_id: s,
                terminal_id: "term-early".into(),
            })
        );
        assert_eq!(plan_registered("term-early", Some(g)), None, "taken once");
    }

    /// A registration with no parked SessionStart plans nothing, and the
    /// later SessionStart then plans it itself.
    #[test]
    fn a_registration_before_session_start_leaves_it_to_the_hook() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        assert_eq!(plan_registered("term-late", Some(g)), None);
        let lookup = move |_: &str| Some(g);
        assert!(plan_session_start("term-late", s, &lookup).is_some());
    }

    /// No claimed attempt in this process (a restored continuation) ⇒ no
    /// fabricated attempt is posted.
    #[test]
    fn an_unknown_attempt_is_never_fabricated() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let lookup = move |_: &str| Some(g);
        assert_eq!(plan_session_start("term-u", Uuid::new_v4(), &lookup), None);
        assert!(plan_respawned(g, "term-u2").is_none());
    }

    /// D4: a respawn forgets the gate's dedupe, so the new pane resuming the
    /// SAME Claude session proves its own start, and opens the barrier.
    #[test]
    fn a_respawn_resets_dedupe_and_opens_the_barrier() {
        let _l = lock();
        reset_for_test();
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 2);
        let lookup = move |_: &str| Some(g);
        assert!(plan_session_start("term-old", s, &lookup).is_some());
        let (post, _gen, _tx) = plan_respawned(g, "term-new").expect("attempt known");
        assert_eq!(
            post,
            ProofPost::Respawned {
                gate_id: g,
                attempt: 2,
                new_terminal_id: "term-new".into()
            }
        );
        assert!(state().respawn_inflight.contains_key(&g));
        assert!(
            plan_session_start("term-new", s, &lookup).is_some(),
            "the new pane's proof is not a duplicate of the old pane's"
        );
    }

    /// Records every post, in order. Answers each post with the next verdict
    /// from `script` (then `Accepted` once it runs out), and delays a respawn
    /// post by `delay` so ordering is tested against a slow reset.
    struct Recorder {
        posts: Arc<StdMutex<Vec<ProofPost>>>,
        script: Arc<StdMutex<VecDeque<PostVerdict>>>,
        delay: Duration,
        started_delay: Duration,
    }

    impl Recorder {
        fn new(delay: Duration, script: Vec<PostVerdict>) -> Self {
            Self {
                posts: Arc::new(StdMutex::new(Vec::new())),
                script: Arc::new(StdMutex::new(script.into())),
                delay,
                started_delay: Duration::ZERO,
            }
        }

        /// Delay every start-proof post too, so a try can be in flight.
        fn with_started_delay(mut self, d: Duration) -> Self {
            self.started_delay = d;
            self
        }
    }

    impl ProofSender for Recorder {
        fn post(&self, post: ProofPost) -> Pin<Box<dyn Future<Output = PostVerdict> + Send>> {
            let posts = self.posts.clone();
            let next = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(PostVerdict::Accepted);
            let delay = match post {
                ProofPost::Respawned { .. } => self.delay,
                ProofPost::Started { .. } => self.started_delay,
            };
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                posts.lock().unwrap().push(post);
                next
            })
        }
    }

    fn rejected(status: u16) -> PostVerdict {
        PostVerdict::Rejected {
            status,
            detail: String::new(),
        }
    }

    #[test]
    fn only_transient_answers_are_retried() {
        for v in [
            PostVerdict::NoAnswer("timeout".into()),
            PostVerdict::SchemaPending,
            rejected(500),
            rejected(503),
        ] {
            assert!(v.is_transient(), "{v:?}");
        }
        for v in [
            PostVerdict::Accepted,
            PostVerdict::NotRecorded,
            PostVerdict::RouteAbsent,
            PostVerdict::Declined {
                status: 409,
                code: "stale_attempt".into(),
            },
            rejected(409),
            rejected(403),
            rejected(422),
        ] {
            assert!(!v.is_transient(), "{v:?}");
        }
        // An idempotent repeat is a 2xx, so it is terminal too.
        assert_eq!(
            classify_response(200, r#"{"accepted":true,"recorded":true,"repeat":true}"#),
            PostVerdict::Accepted
        );
    }

    fn a_post() -> ProofPost {
        ProofPost::Started {
            gate_id: gate(),
            attempt: 1,
            claude_session_id: Uuid::new_v4(),
            terminal_id: "term-x".into(),
        }
    }

    /// Transient answers are retried until a terminal one, within the budget.
    #[tokio::test(start_paused = true)]
    async fn transient_answers_retry_until_terminal() {
        let rec = Recorder::new(
            Duration::ZERO,
            vec![
                rejected(500),
                PostVerdict::SchemaPending,
                PostVerdict::Accepted,
            ],
        );
        let (v, tries) = post_with_retry(&rec, &a_post(), 0).await;
        assert_eq!((v, tries), (Some(PostVerdict::Accepted), 3));
    }

    /// The budget is bounded: the last transient answer is returned.
    #[tokio::test(start_paused = true)]
    async fn the_retry_budget_is_bounded() {
        let rec = Recorder::new(
            Duration::ZERO,
            vec![
                PostVerdict::NoAnswer("a".into()),
                rejected(503),
                PostVerdict::NoAnswer("c".into()),
                PostVerdict::Accepted,
            ],
        );
        let (v, tries) = post_with_retry(&rec, &a_post(), 0).await;
        assert_eq!(tries, RETRY_DELAYS.len() + 1);
        assert_eq!(v, Some(PostVerdict::NoAnswer("c".into())));
    }

    /// A 4xx — and a repeat, a stale attempt, a declined race — is never retried.
    #[tokio::test(start_paused = true)]
    async fn a_terminal_answer_is_not_retried() {
        for first in [
            rejected(409),
            rejected(403),
            PostVerdict::RouteAbsent,
            PostVerdict::NotRecorded,
            PostVerdict::Declined {
                status: 409,
                code: "stale_attempt".into(),
            },
            classify_response(200, r#"{"accepted":true,"recorded":true,"repeat":true}"#),
        ] {
            let rec = Recorder::new(Duration::ZERO, vec![first.clone(), PostVerdict::Accepted]);
            let (v, tries) = post_with_retry(&rec, &a_post(), 0).await;
            assert_eq!((v, tries), (Some(first), 1));
        }
    }

    /// Wait in (paused) virtual time until `n` posts exist — up to 150 s, past
    /// [`RESPAWN_BARRIER_WAIT`] plus every retry delay — and panic naming
    /// the posts it saw if they never come.
    async fn settle(posts: &Arc<StdMutex<Vec<ProofPost>>>, n: usize) {
        for _ in 0..150 {
            if posts.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        panic!(
            "expected {n} posts within 150 s of virtual time, saw {:?}",
            posts.lock().unwrap()
        );
    }

    /// A2: the old pane's start proof, asleep in a 5xx backoff when the gate
    /// is respawned, is dropped on wake — not sent, and its un-mark skipped so
    /// the new pane's dedupe key survives.
    #[tokio::test(start_paused = true)]
    async fn a_stale_old_pane_retry_is_dropped_across_a_respawn() {
        let _l = lock();
        reset_for_test();
        let rec = Recorder::new(Duration::ZERO, vec![rejected(500)]);
        let posts = rec.posts.clone();
        set_sender_for_test(Arc::new(rec));
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        let lookup = move |_: &str| Some(g);
        let planned = plan_session_start("term-fence-old", s, &lookup).expect("planned");
        let old = planned.post.clone();
        dispatch(planned);
        settle(&posts, 1).await; // try 1 answered 500; now in backoff

        report_respawned(Some(g), "term-fence-new");
        // The new pane's proof re-marks the same (gate, attempt, session).
        assert!(plan_session_start("term-fence-new", s, &lookup).is_some());
        settle(&posts, 2).await; // the reset
        tokio::time::sleep(RESPAWN_BARRIER_WAIT * 2).await; // past every backoff

        set_sender_for_test(Arc::new(HttpProofSender));
        let got = posts.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                old,
                ProofPost::Respawned {
                    gate_id: g,
                    attempt: 0,
                    new_terminal_id: "term-fence-new".into()
                },
            ],
            "the old pane's retry must not be sent after the respawn"
        );
        assert!(
            state()
                .sent
                .contains(&(g, 0, s, "term-fence-new".to_string())),
            "the dropped retry must not un-mark the new pane's key"
        );
    }

    /// The old pane's LAST try is in flight when the gate is respawned: its
    /// answer (here a transient one) is discarded, so it neither un-marks the
    /// new pane's key nor counts as this gate's verdict.
    #[tokio::test(start_paused = true)]
    async fn a_last_try_in_flight_across_a_respawn_is_discarded() {
        let _l = lock();
        reset_for_test();
        let rec = Recorder::new(
            Duration::ZERO,
            vec![
                rejected(500),
                rejected(500),
                PostVerdict::NoAnswer("timeout".into()),
            ],
        )
        .with_started_delay(Duration::from_secs(5));
        let posts = rec.posts.clone();
        set_sender_for_test(Arc::new(rec));
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        let lookup = move |_: &str| Some(g);
        let planned = plan_session_start("term-last-old", s, &lookup).expect("planned");
        let old = planned.post.clone();
        dispatch(planned);
        // Tries answer at 5 s and 20+5 s; the last starts after the 40 s
        // backoff (t=65 s) and answers at t=70 s. Respawn inside that window.
        tokio::time::sleep(Duration::from_secs(67)).await;
        assert_eq!(posts.lock().unwrap().len(), 2, "two tries answered so far");
        report_respawned(Some(g), "term-last-new");
        assert!(plan_session_start("term-last-new", s, &lookup).is_some());
        settle(&posts, 4).await;
        tokio::time::sleep(Duration::from_secs(10)).await;

        set_sender_for_test(Arc::new(HttpProofSender));
        let got = posts.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                old.clone(),
                old.clone(),
                ProofPost::Respawned {
                    gate_id: g,
                    attempt: 0,
                    new_terminal_id: "term-last-new".into()
                },
                old,
            ]
        );
        assert!(
            state()
                .sent
                .contains(&(g, 0, s, "term-last-new".to_string())),
            "the discarded last answer must not un-mark the new pane's key"
        );
    }

    /// End to end through the real continuation registry: the SessionStart
    /// hook posts for a registered continuation terminal, a respawn's reset
    /// reaches the sender BEFORE the new pane's proof even when the reset is
    /// slow, and nothing posts for an operator tab.
    #[tokio::test(start_paused = true)]
    async fn the_respawn_reset_is_posted_before_the_new_panes_proof() {
        let _reg = crate::agent_runtime::CONTINUATION_REGISTRY_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let _l = lock();
        reset_for_test();
        // The reset first meets a 503 and is retried; the new pane's proof
        // must still follow it.
        let rec = Recorder::new(
            Duration::from_millis(150),
            vec![PostVerdict::Accepted, rejected(503)],
        );
        let posts = rec.posts.clone();
        set_sender_for_test(Arc::new(rec));
        let g = gate();
        let s = Uuid::new_v4();
        record_dispatch_attempt(g, 0);
        let old = format!("term-e2e-old-{g}");
        let new = format!("term-e2e-new-{g}");
        crate::agent_runtime::register_continuation_for_proof_test(old.clone(), g);

        on_session_start(&old, Some(s));
        on_session_start("term-e2e-operator", Some(Uuid::new_v4()));
        settle(&posts, 1).await;

        // The migration's order: report, then re-pin; the new pane's
        // SessionStart parks first (it started during the respawn).
        on_session_start(&new, Some(s));
        report_respawned(Some(g), &new);
        crate::agent_runtime::deregister_continuation_for_test(&old);
        crate::agent_runtime::register_continuation_for_proof_test(new.clone(), g);
        settle(&posts, 4).await;

        crate::agent_runtime::deregister_continuation_for_test(&new);
        set_sender_for_test(Arc::new(HttpProofSender));
        let got = posts.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                ProofPost::Started {
                    gate_id: g,
                    attempt: 0,
                    claude_session_id: s,
                    terminal_id: old.clone(),
                },
                ProofPost::Respawned {
                    gate_id: g,
                    attempt: 0,
                    new_terminal_id: new.clone()
                },
                ProofPost::Respawned {
                    gate_id: g,
                    attempt: 0,
                    new_terminal_id: new.clone()
                },
                // The new pane's proof names the NEW pane — the same id the
                // reset sent as new_terminal_id.
                ProofPost::Started {
                    gate_id: g,
                    attempt: 0,
                    claude_session_id: s,
                    terminal_id: new.clone(),
                },
            ]
        );
    }

    /// The hook fires in every `QONTINUI_POLICY_INJECTION` mode because the
    /// route calls it BEFORE the call that branches on the mode (`off` and
    /// `observe` return from inside `policy_context::policy_context`).
    #[test]
    fn the_hook_runs_before_the_injection_mode_branch() {
        let src = include_str!("mcp/sessions.rs");
        let (_, handler) = src
            .split_once("async fn policy_context(")
            .expect("policy_context handler");
        let hook = handler
            .find("continuation_start_proof::on_session_start(")
            .expect("the handler calls the start-proof hook");
        let branch = handler
            .find("crate::mcp::policy_context::policy_context(")
            .expect("the handler calls the mode-branching fn");
        assert!(hook < branch, "the hook must run before the mode branch");
    }

    /// The migration reports the respawn before it re-pins; the resume door,
    /// which registers no continuation on its pane, reports none.
    #[test]
    fn re_pin_sites_report_the_respawn_before_the_re_pin() {
        let src = include_str!("terminal/account_migration.rs");
        let (_, ok_arm) = src
            .split_once("Ok((terminal_id, _coord_session_id)) => {")
            .expect("migrate_session's Ok arm");
        let report = ok_arm
            .find("continuation_start_proof::report_respawned(")
            .expect("migrate_session reports the respawn");
        let repin = ok_arm
            .find("restore_continuation_registration(")
            .expect("migrate_session re-pins");
        assert!(report < repin, "report before re-pin");

        let door = include_str!("session/resume_door.rs");
        assert!(
            !door.contains("continuation_start_proof::report_respawned("),
            "the resume door must not reset a proof nothing on its pane can re-prove"
        );
    }
}
