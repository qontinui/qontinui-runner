//! Session RUN records → coord's durable run ledger (`coord.session_runs`).
//! Plan `2026-10-23-session-duration-by-work-type-and-per-device-session-load`,
//! Phase 4 (runner side). Sibling of [`super::restore_record_emitter`], and
//! modelled on it: gated, debounced, failure-swallowing, written into the SAME
//! local-first outbox the [`super::coord_sync`] drain reads.
//!
//! ## What a run is
//!
//! One open→close interval of one [`SessionLifecycleStore`] record (plan D1). A
//! record re-opened after a close (boot resume, handoff adoption) keeps its
//! `claude_session_id` and becomes run `seq + 1`, so wall time is never
//! double-counted and a resume is visible as such. The run counter lives ON the
//! record ([`RunAccounting`], persisted through the store's WAL), so it survives
//! a runner restart.
//!
//! A run is COUNTED only once the record is confirmed (`confirmed_at` set — the
//! provider's SessionStart hook, or an observed transcript bind, proved a real
//! provider session runs there). The identity seam writes a provisional record
//! for every PTY, including a plain shell that never runs `claude`; counting
//! those would fill the ledger with phantom one-second "sessions". A record
//! that is open and confirmed but was never counted (it predates this module)
//! is counted at its close, so its end still reaches coord.
//!
//! The agentic plane ([`crate::claude_session::manager::SessionManager`],
//! registered through `claude_session/coord_register.rs`) has no store record
//! that is ever confirmed, so it emits the same events through
//! [`RunRecordEmitter::agentic_start`] / [`RunRecordEmitter::agentic_end`],
//! with its own durable per-session counter ([`AgenticRunLedger`]). Both planes
//! key the run on `(claude_code_session_id, run_seq)`, which is coord's
//! idempotency key, so a session seen by both lands on ONE row.
//!
//! ## Wire
//!
//! Each event is one `session_run` outbox row whose payload is
//! `{"tenant_id"?: <owning tenant>, "body": <wire body>}`. The drain POSTs
//! `body` verbatim to `POST {coord}/coord/session-runs` with device auth; the
//! `tenant_id` only selects which device-JWT slot presents it
//! (`coord_sync::record_session_tenant`), exactly as for every other kind.
//!
//! ```json
//! {
//!   "claude_code_session_id": "<uuid>",
//!   "run_seq": 1,
//!   "event": "start" | "merge" | "end",
//!   "started_at": "<RFC3339>",
//!   "ended_at": "<RFC3339>",        // end only
//!   "close_reason": "<raw store close_reason>", // end only
//!   "last_seen_at": "<RFC3339>",
//!   "facts": { "spawn_origin", "spawn_kind", "gate_id", "agent_id",
//!              "coord_session_id", "spawn_request_id", "skill",
//!              "first_command", "bind_origin", "account_label", "provider" }
//! }
//! ```
//!
//! Every event is SELF-CONTAINED — `started_at` and the whole fact set ride
//! every one — so coord can create the row from whichever arrives first (an
//! `end` for a run whose `start` was ACK-dropped still makes a complete row).
//! `close_reason` is the store's RAW value; coord maps it to `end_reason`.
//!
//! NO conversation content rides this event. `first_command` is a command NAME
//! validated against `^[a-z0-9:_-]{1,64}$` ([`valid_first_command`]) — never
//! its arguments, never prompt text (plan D3).
//!
//! ## Gates
//!
//! 1. **Runner-global** — `Settings.session_metadata_sync_enabled`, the same
//!    consent the restore-record mirror reads (this is session METADATA, not
//!    transcript content). Off ⇒ no outbox row, nothing leaves the machine.
//! 2. **Id shape** — coord types `claude_code_session_id` as a uuid; a record
//!    keyed by anything else is skipped (one debug line per id).
//! 3. **Debounce** — an event whose body (less `last_seen_at`) equals the last
//!    one emitted for that run is not re-emitted. In-memory: a restart re-emits
//!    at most once, and coord's ingest is an idempotent upsert.
//!
//! ## Failure posture
//!
//! Best-effort throughout: every failure is logged and swallowed, and nothing
//! here can fail the store write that triggered it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as JsonValue};
use uuid::Uuid;

use super::local_store::OutboxWriter;
use super::session_lifecycle_store::{
    TerminalSessionRecord, ORIGIN_AUTHORITATIVE, ORIGIN_OBSERVED, ORIGIN_RECONCILED,
};
use super::SessionEventKind;
use crate::coord_drain_state::SpawnOrigin;

/// Wire kind of the outbox row. Matches [`SessionEventKind::SessionRun`].
pub const SESSION_RUN_EVENT: &str = "session_run";

/// Coord route the drain POSTs [`SESSION_RUN_EVENT`] bodies to.
pub const SESSION_RUNS_ROUTE: &str = "/coord/session-runs";

/// Close reason an agentic-plane run ends with: `close_session` carries no
/// reason of its own, and coord keeps an unmapped raw value under `other`.
pub const AGENTIC_CLOSE_REASON: &str = "agentic-close";

/// Namespace for the outbox lane a session's run rows ride. A dedicated lane
/// (UUIDv5 of the claude session id) keeps run rows from ever sharing a seq
/// chain with a `coord.sessions` lane, so a run row that is failing can only
/// hold back other run rows of the same session.
const RUN_LANE_NAMESPACE: Uuid = Uuid::from_u128(0x5e55_1072_c0de_4a11_9b2f_7d1e_aa55_0004);

/// The outbox lane for `claude_session_id`'s run rows.
pub fn run_lane(claude_session_id: &str) -> Uuid {
    Uuid::new_v5(
        &RUN_LANE_NAMESPACE,
        format!("session-run:{claude_session_id}").as_bytes(),
    )
}

/// The three wire events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunEvent {
    /// A run began (the record became open + confirmed).
    Start,
    /// A fact learned after the start (spawn facts stamped late, the first
    /// slash command read from the transcript).
    Merge,
    /// The run ended (the store closed the record).
    End,
}

impl RunEvent {
    pub fn as_wire(self) -> &'static str {
        match self {
            RunEvent::Start => "start",
            RunEvent::Merge => "merge",
            RunEvent::End => "end",
        }
    }
}

// ---------------------------------------------------------------------------
// Spawn facts
// ---------------------------------------------------------------------------

/// Why a session was launched, as the LAUNCHER knew it (plan D2's raw facts).
/// Carried on [`TerminalSessionRecord::spawn`] for the terminal plane and handed
/// to [`RunRecordEmitter::agentic_start`] for the agentic plane. Coord enriches
/// the rest (the gate's `registered_by`, the worktree's `dispatch_source`) at
/// ingest from rows it already owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnFacts {
    /// [`SpawnOrigin`] — serialized as its wire name (`gate_continuation`, …).
    pub spawn_origin: SpawnOrigin,
    /// The coord gate whose continuation launched this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_id: Option<String>,
    /// The coord agent id the session's worktree allocation minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// A finer launcher-side kind (`condition_check`, `remote_create`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_kind: Option<String>,
    /// The skill / slash command the launcher asked the session to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The coord spawn-request id, when the launcher holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_request_id: Option<String>,
}

fn non_blank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl SpawnFacts {
    pub fn new(spawn_origin: SpawnOrigin) -> Self {
        Self {
            spawn_origin,
            gate_id: None,
            agent_id: None,
            spawn_kind: None,
            skill: None,
            spawn_request_id: None,
        }
    }

    pub fn gate_id(mut self, v: Option<String>) -> Self {
        self.gate_id = non_blank(v);
        self
    }

    pub fn agent_id(mut self, v: Option<String>) -> Self {
        self.agent_id = non_blank(v);
        self
    }

    pub fn spawn_kind(mut self, v: &str) -> Self {
        self.spawn_kind = non_blank(Some(v.to_string()));
        self
    }

    pub fn skill(mut self, v: Option<String>) -> Self {
        self.skill = non_blank(v);
        self
    }

    pub fn spawn_request_id(mut self, v: Option<String>) -> Self {
        self.spawn_request_id = non_blank(v);
        self
    }
}

/// The skill a launch prompt asks for, when the prompt IS a slash command
/// (`/vet-imp plans/x.md` → `vet-imp`): the first token, validated exactly as
/// `first_command` is ([`valid_first_command`]). Anything else is `None` —
/// coord enriches the skill from the gate's `continuation_spawn` itself.
/// Arguments never leave this function.
pub fn skill_of_prompt(prompt: &str) -> Option<String> {
    let first = prompt.split_whitespace().next()?;
    if !first.starts_with('/') {
        return None;
    }
    valid_first_command(first)
}

/// Stamp `facts` on the record the spawn just created on `terminal_id` — the
/// one call every terminal launch site makes after its terminal exists (the
/// `runner_spawn_sites` guard checks for it). `since_ms` is the instant the
/// caller took BEFORE creating the terminal. No store managed (headless) ⇒
/// `false`.
pub fn stamp_spawn_facts(
    app: &tauri::AppHandle,
    terminal_id: &str,
    facts: SpawnFacts,
    since_ms: i64,
) -> bool {
    use tauri::Manager;
    app.try_state::<Arc<super::session_lifecycle_store::SessionLifecycleStore>>()
        .is_some_and(|store| store.record_spawn_facts(terminal_id, facts, since_ms))
}

/// The instant a launch site takes before creating its terminal, for
/// [`stamp_spawn_facts`].
pub fn spawn_clock_ms() -> i64 {
    Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// Per-record run accounting (persisted on the record)
// ---------------------------------------------------------------------------

/// The run counter a [`TerminalSessionRecord`] carries. Persisted through the
/// store's WAL so `seq` survives a restart; `#[serde(default)]` on the record
/// field makes every record written before this existed load as "no run yet".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAccounting {
    /// The current (or last) run's `run_seq`; 0 = no run counted yet.
    #[serde(default)]
    pub seq: u32,
    /// Unix millis the record's CURRENT open interval began — set when the
    /// store inserts the record or re-opens a closed one. `None` on a record
    /// that predates the field (the record's `opened_at` stands in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<i64>,
    /// The current open interval has been counted as run `seq`.
    #[serde(default)]
    pub started: bool,
    /// Run `seq`'s END has been emitted — so a later close of the same record
    /// (a terminal-only tile closed long after the runner restart ended its
    /// run) can never emit a second, later end for it.
    #[serde(default)]
    pub ended: bool,
    /// Run `seq`'s START has been emitted. A run that begins before its
    /// launcher's facts land (the launch site stamps them right AFTER its
    /// terminal exists) HOLDS its start until the stamp arrives — or until the
    /// first liveness touch, so a run no launcher ever stamps is still visible
    /// while it runs. See `SessionLifecycleStore::emit_start_or_hold`.
    #[serde(default)]
    pub start_emitted: bool,
    /// The session's first slash command, read from its transcript (plan D3).
    /// Already validated by [`valid_first_command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_command: Option<String>,
}

impl RunAccounting {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The record entered `open` (inserted, or re-opened after a close): a new
    /// open interval begins, not yet counted.
    pub fn note_opened(&mut self, now_ms: i64) {
        self.opened_at = Some(now_ms);
        self.started = false;
        self.ended = false;
        self.start_emitted = false;
    }
}

/// Whether the counted run's START is due NOW — claimed (marked emitted) when it
/// is. A start is held while the record carries no launch facts yet, because
/// the launch site stamps them right after its terminal exists and the start
/// should carry them; `release_held` lets the first liveness touch release a
/// run no launcher stamps (an operator's bare `claude`), so it is never
/// invisible while it runs.
pub fn claim_start(rec: &mut TerminalSessionRecord, release_held: bool) -> bool {
    let due = rec.run.started
        && !rec.run.start_emitted
        && !rec.run.ended
        && (rec.spawn.is_some() || release_held);
    if due {
        rec.run.start_emitted = true;
    }
    due
}

/// Mark the counted run's end as emitted. `true` exactly once per counted run
/// — the caller emits [`RunEvent::End`] for the record when it returns `true`.
/// Every close path calls [`begin_run_if_due`] first (on the still-open record)
/// so an open+confirmed record that predates run accounting is counted at its
/// close.
pub fn end_run_if_due(rec: &mut TerminalSessionRecord) -> bool {
    if !rec.run.started || rec.run.ended {
        return false;
    }
    rec.run.ended = true;
    true
}

/// The close reason a run carries when the runner that hosted it stopped and a
/// LATER runner process re-materialized the record (boot restore). Raw, like
/// every other store close reason; coord maps it (`runner_stopped`).
pub const RUNNER_STOPPED_CLOSE_REASON: &str = "runner-stopped";

/// Unix millis this runner process's run accounting started — the boundary a
/// boot restore compares a run's start against. Initialized by
/// `SessionLifecycleStore::open`, which runs at boot.
pub fn process_boot_ms() -> i64 {
    static BOOT_MS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *BOOT_MS.get_or_init(|| Utc::now().timestamp_millis())
}

/// A boot restore is re-materializing `rec`, which a PREVIOUS runner process
/// was running. That process's run ended when it stopped (D4 `runner_stopped`):
/// end it — once — at the last moment that process saw it alive. Returns the
/// ended snapshot to emit [`RunEvent::End`] for.
///
/// It never STARTS the next run: a resume is a new run only once its handshake
/// is verified (the provider's confirming hook re-asserting the record, or
/// `clear_restore_pending`) — see [`begin_next_interval`]. A terminal-only
/// restore starts none.
///
/// Idempotent per boot: once the boundary ran the run is ended, and an
/// interval this process opened is never ended here.
pub fn boot_restore_boundary(
    rec: &mut TerminalSessionRecord,
    boot_ms: i64,
) -> Option<TerminalSessionRecord> {
    if rec.state != "open" || rec.run.opened_at.unwrap_or(rec.opened_at) >= boot_ms {
        return None;
    }
    // Count a confirmed-but-uncounted previous run so its end is not lost.
    begin_run_if_due(rec);
    if !end_run_if_due(rec) {
        return None;
    }
    let mut ended = rec.clone();
    ended.state = "closed".to_string();
    ended.closed_at = Some(rec.last_seen_at.min(boot_ms));
    ended.close_reason = Some(RUNNER_STOPPED_CLOSE_REASON.to_string());
    Some(ended)
}

/// The end time to REPORT for a run closed now. A run a previous process
/// opened, and that this process never saw alive (`last_seen_at` before the
/// boot), ended at `min(last_seen_at, boot)` — not at the moment this process
/// got round to closing it. `None` ⇒ the store's own `closed_at` stands.
pub fn previous_process_end_ms(rec: &TerminalSessionRecord, boot_ms: i64) -> Option<i64> {
    let opened = rec.run.opened_at.unwrap_or(rec.opened_at);
    (opened < boot_ms && rec.last_seen_at < boot_ms).then(|| rec.last_seen_at.min(boot_ms))
}

/// Open the record's NEXT run interval on an OPEN record whose run already
/// ended (a resume verified after a boot restore, or a resume typed into a
/// terminal-only tile). The launch facts are this interval's: a boot-restored
/// record keeps its work's facts under origin `boot_resume`; any other re-open
/// drops the old facts so the launcher's stamp (or terminal inheritance) fills
/// them. Returns whether a run began (it needs confirmation, as every run).
pub fn begin_next_interval(rec: &mut TerminalSessionRecord, now_ms: i64, boot_ms: i64) -> bool {
    if rec.state != "open" || !rec.run.ended {
        return false;
    }
    rec.run.note_opened(now_ms);
    // Only a resume the RUNNER started (its boot restore typed the resume:
    // tier `resumed`, or `failed` while its restore is still pending) continues
    // the same work. A resume typed into a terminal-only tile — or into a tile
    // whose runner resume already failed for good — is a new launch: fresh
    // facts.
    let runner_resumed = rec.restored_from_boot_at.is_some_and(|t| t >= boot_ms)
        && match rec.restore_tier.as_deref() {
            Some(super::session_lifecycle_store::RESTORE_TIER_RESUMED) => true,
            Some(super::session_lifecycle_store::RESTORE_TIER_FAILED) => {
                rec.restore_pending_at.is_some()
            }
            _ => false,
        };
    if runner_resumed {
        match rec.spawn.as_mut() {
            Some(sp) => sp.spawn_origin = SpawnOrigin::BootResume,
            None => rec.spawn = Some(SpawnFacts::new(SpawnOrigin::BootResume)),
        }
    } else {
        rec.spawn = None;
    }
    begin_run_if_due(rec)
}

/// The wire spelling of a raw store close reason: kept when it matches
/// `^[a-z0-9_:.-]{1,64}$`, else `other` — a free-text reason (the orchestration
/// teardown's sentence, say) never rides the wire.
pub fn wire_close_reason(raw: &str) -> &str {
    let ok = (1..=64).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_:.-".contains(&b));
    if ok {
        raw
    } else {
        "other"
    }
}

/// Count the record's current open interval as a run when it is due: open,
/// confirmed, not yet counted. Returns `true` when this call began the run.
pub fn begin_run_if_due(rec: &mut TerminalSessionRecord) -> bool {
    if rec.state != "open" || rec.confirmed_at.is_none() || rec.run.started {
        return false;
    }
    rec.run.seq = rec.run.seq.saturating_add(1);
    rec.run.started = true;
    if rec.run.opened_at.is_none() {
        rec.run.opened_at = Some(rec.opened_at);
    }
    true
}

/// `first_command` validation (plan D3): the command NAME only, lowercase
/// `[a-z0-9:_-]`, 1–64 chars, leading `/` stripped. Anything else is dropped —
/// never truncated, so a partial argument can never leak through.
pub fn valid_first_command(raw: &str) -> Option<String> {
    // No trim: a name carrying whitespace is malformed and dropped whole.
    let name = raw.strip_prefix('/').unwrap_or(raw);
    let ok = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_:".contains(&b));
    ok.then(|| name.to_string())
}

// ---------------------------------------------------------------------------
// The snapshot both planes build their body from
// ---------------------------------------------------------------------------

/// Everything one wire body needs, independent of which plane produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSnapshot {
    pub claude_session_id: String,
    pub run_seq: u32,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub close_reason: Option<String>,
    pub last_seen_at_ms: Option<i64>,
    pub spawn: Option<SpawnFacts>,
    pub first_command: Option<String>,
    pub bind_origin: Option<&'static str>,
    pub account_label: Option<String>,
    pub provider: Option<String>,
    pub coord_session_id: Option<Uuid>,
    pub tenant_id: Option<String>,
}

/// The store's `origin` in the contract's two-value `bind_origin`
/// vocabulary. `observed` (a process-anchored transcript bind, resume-safe)
/// reads as `authoritative`; an absent origin is read as `reconciled`, exactly
/// as every other consumer of the field reads it.
fn bind_origin_of(origin: Option<&str>) -> &'static str {
    match origin {
        Some(ORIGIN_AUTHORITATIVE) | Some(ORIGIN_OBSERVED) => ORIGIN_AUTHORITATIVE,
        _ => ORIGIN_RECONCILED,
    }
}

impl RunSnapshot {
    /// The snapshot of `rec`'s current (or just-closed) run. `None` when no run
    /// has been counted on the record.
    pub fn from_record(
        rec: &TerminalSessionRecord,
        coord_session_id: Option<Uuid>,
    ) -> Option<Self> {
        if rec.run.seq == 0 {
            return None;
        }
        let closed = rec.state == "closed";
        Some(Self {
            claude_session_id: rec.claude_session_id.clone(),
            run_seq: rec.run.seq,
            started_at_ms: rec.run.opened_at.unwrap_or(rec.opened_at),
            ended_at_ms: if closed { rec.closed_at } else { None },
            close_reason: if closed {
                rec.close_reason.clone()
            } else {
                None
            },
            last_seen_at_ms: Some(rec.last_seen_at),
            spawn: rec.spawn.clone(),
            first_command: rec.run.first_command.clone(),
            bind_origin: Some(bind_origin_of(rec.origin.as_deref())),
            account_label: rec.account_label.clone(),
            provider: Some(rec.provider.clone()),
            coord_session_id,
            tenant_id: rec.tenant_id.clone(),
        })
    }
}

fn rfc3339(ms: i64) -> Option<String> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|t: DateTime<Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// Build the binding wire body. Pure.
///
/// Event ORDER does not matter to coord: its ingest is an idempotent upsert on
/// `(tenant, device, claude_code_session_id, run_seq)` and every event is
/// self-contained, so an `end` that overtakes its `start` (or a `merge`) still
/// builds the same row. `None` when the session id is not a
/// uuid (coord types the field `Uuid`) or a required timestamp is out of
/// range.
pub fn run_record_body(snap: &RunSnapshot, event: RunEvent) -> Option<JsonValue> {
    let ccsid = Uuid::parse_str(snap.claude_session_id.trim()).ok()?;
    let mut facts = Map::new();
    let mut put = |k: &str, v: Option<String>| {
        if let Some(v) = v.filter(|s| !s.is_empty()) {
            facts.insert(k.to_string(), JsonValue::String(v));
        }
    };
    if let Some(sp) = &snap.spawn {
        put("spawn_origin", Some(sp.spawn_origin.as_wire().to_string()));
        put("spawn_kind", sp.spawn_kind.clone());
        put("gate_id", sp.gate_id.clone());
        put("agent_id", sp.agent_id.clone());
        put("spawn_request_id", sp.spawn_request_id.clone());
        put("skill", sp.skill.clone());
    }
    put(
        "coord_session_id",
        snap.coord_session_id.map(|u| u.to_string()),
    );
    put("first_command", snap.first_command.clone());
    put("bind_origin", snap.bind_origin.map(str::to_string));
    put("account_label", snap.account_label.clone());
    put("provider", snap.provider.clone());

    let mut body = json!({
        "claude_code_session_id": ccsid,
        "run_seq": snap.run_seq,
        "event": event.as_wire(),
        "started_at": rfc3339(snap.started_at_ms)?,
        "facts": JsonValue::Object(facts),
    });
    if let Some(seen) = snap.last_seen_at_ms.and_then(rfc3339) {
        body["last_seen_at"] = json!(seen);
    }
    if event == RunEvent::End {
        // An end with no observed close time is not an observed end: say
        // nothing rather than invent one (coord's backstop owns that case).
        body["ended_at"] = json!(rfc3339(snap.ended_at_ms?)?);
        if let Some(r) = snap.close_reason.as_deref().filter(|r| !r.is_empty()) {
            body["close_reason"] = json!(wire_close_reason(r));
        }
    }
    Some(body)
}

// ---------------------------------------------------------------------------
// Agentic-plane ledger (durable run_seq for sessions with no store record)
// ---------------------------------------------------------------------------

/// One agentic-plane session's run counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgenticRun {
    pub seq: u32,
    pub opened_at: i64,
    /// The run's last known activity (its start, then each operator
    /// interaction the registrar reports). A run a previous process left open
    /// is ended HERE, never at the moment a later process noticed.
    #[serde(default)]
    pub last_seen_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// The raw close reason (`agentic-close`, or `runner-stopped` for a run a
    /// previous process left open).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<SpawnFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// `run` as a previous process left it, ended at its last known activity
/// (clamped to the boot) with [`RUNNER_STOPPED_CLOSE_REASON`].
fn close_previous_process_run(run: &AgenticRun, boot_ms: i64) -> AgenticRun {
    let mut ended = run.clone();
    ended.closed_at = Some(run.last_seen_at.max(run.opened_at).min(boot_ms));
    ended.close_reason = Some(RUNNER_STOPPED_CLOSE_REASON.to_string());
    ended
}

/// Closed agentic runs older than this are pruned from the ledger.
const AGENTIC_LEDGER_RETENTION_MS: i64 = 30 * 24 * 3600 * 1000;

/// Durable `claude_session_id → AgenticRun` map. The agentic plane's dedupe
/// index is in-memory, so a session resumed after a restart re-registers — this
/// file is what makes that re-registration run `seq + 1` instead of a second
/// `run_seq = 1` that would overwrite the first run's row. `None` path ⇒ an
/// in-memory ledger (tests, ephemeral fallbacks).
#[derive(Debug)]
pub struct AgenticRunLedger {
    path: Option<PathBuf>,
    map: Mutex<HashMap<String, AgenticRun>>,
}

impl AgenticRunLedger {
    pub fn open(path: Option<PathBuf>) -> Self {
        let map = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<HashMap<String, AgenticRun>>(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    fn persist(&self, map: &HashMap<String, AgenticRun>) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(map).unwrap_or_default())?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "run_record_emitter: agentic run ledger write failed (best-effort)"
            );
        }
    }

    /// Open a new run for `ccsid`: `seq + 1` of the last recorded run, or 1.
    /// When the last run is still OPEN and a previous process opened it, that
    /// run ended when the process stopped: it is closed first (`runner-stopped`
    /// at its last known activity) and returned so its end is emitted. With no
    /// facts given, the previous run's facts carry over (a re-registration of
    /// the same work).
    pub fn open_run(
        &self,
        ccsid: &str,
        now_ms: i64,
        boot_ms: i64,
        spawn: Option<SpawnFacts>,
        tenant_id: Option<String>,
    ) -> (Option<AgenticRun>, AgenticRun) {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = m.get(ccsid).cloned();
        let ended = prev
            .as_ref()
            .filter(|r| r.closed_at.is_none() && r.opened_at < boot_ms)
            .map(|r| close_previous_process_run(r, boot_ms));
        let seq = prev.as_ref().map(|r| r.seq).unwrap_or(0).saturating_add(1);
        let run = AgenticRun {
            seq,
            opened_at: now_ms,
            last_seen_at: now_ms,
            closed_at: None,
            close_reason: None,
            spawn: spawn.or_else(|| prev.as_ref().and_then(|r| r.spawn.clone())),
            tenant_id: tenant_id.or_else(|| prev.as_ref().and_then(|r| r.tenant_id.clone())),
        };
        m.insert(ccsid.to_string(), run.clone());
        m.retain(|_, r| {
            r.closed_at
                .is_none_or(|c| now_ms - c <= AGENTIC_LEDGER_RETENTION_MS)
        });
        self.persist(&m);
        (ended, run)
    }

    /// Close `ccsid`'s open run. `None` when it has no open run.
    pub fn close_run(&self, ccsid: &str, now_ms: i64) -> Option<AgenticRun> {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let run = m.get_mut(ccsid).filter(|r| r.closed_at.is_none())?;
        run.closed_at = Some(now_ms);
        run.last_seen_at = run.last_seen_at.max(now_ms);
        run.close_reason = Some(AGENTIC_CLOSE_REASON.to_string());
        let closed = run.clone();
        self.persist(&m);
        Some(closed)
    }

    /// [`Self::touch`] for several runs, persisted ONCE.
    pub fn touch_many<'a>(&self, ccsids: impl IntoIterator<Item = &'a str>, now_ms: i64) {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;
        for ccsid in ccsids {
            if let Some(run) = m.get_mut(ccsid).filter(|r| r.closed_at.is_none()) {
                run.last_seen_at = now_ms;
                changed = true;
            }
        }
        if changed {
            self.persist(&m);
        }
    }

    /// Bump `ccsid`'s open run's last known activity. No write when it has none.
    pub fn touch(&self, ccsid: &str, now_ms: i64) {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(run) = m.get_mut(ccsid).filter(|r| r.closed_at.is_none()) {
            run.last_seen_at = now_ms;
            self.persist(&m);
        }
    }

    /// The open runs THIS process opened (`opened_at >= boot_ms`).
    pub fn current_open_runs(&self, boot_ms: i64) -> Vec<(String, AgenticRun)> {
        let m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        m.iter()
            .filter(|(_, r)| r.closed_at.is_none() && r.opened_at >= boot_ms)
            .map(|(k, r)| (k.clone(), r.clone()))
            .collect()
    }

    /// End every run a PREVIOUS process left open (`runner-stopped`, at its
    /// last known activity) — the boot sweep, so a session that is never
    /// re-registered does not stay open on coord forever. Returns the
    /// `(ccsid, ended run)` pairs to emit.
    pub fn end_previous_process_runs(&self, boot_ms: i64) -> Vec<(String, AgenticRun)> {
        let mut m = self
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ended = Vec::new();
        for (ccsid, run) in m.iter_mut() {
            if run.closed_at.is_none() && run.opened_at < boot_ms {
                *run = close_previous_process_run(run, boot_ms);
                ended.push((ccsid.clone(), run.clone()));
            }
        }
        if !ended.is_empty() {
            self.persist(&m);
        }
        ended
    }

    #[cfg(test)]
    fn get(&self, ccsid: &str) -> Option<AgenticRun> {
        self.map.lock().ok()?.get(ccsid).cloned()
    }
}

// ---------------------------------------------------------------------------
// The emitter
// ---------------------------------------------------------------------------

/// Gate-1 probe (`Settings.session_metadata_sync_enabled`), injectable.
pub type ConsentGate = Box<dyn Fn() -> bool + Send + Sync>;

/// Resolves a hosting `terminal_id` to the coord session mirroring it — the
/// same injected lookup the restore-record emitter uses. Feeds the optional
/// `coord_session_id` fact.
pub type CoordSessionResolver = Box<dyn Fn(&str) -> Option<Uuid> + Send + Sync>;

/// The debounce key value: the last body emitted for a run, less
/// `last_seen_at` (which moves on every touch and is not a material change).
type DebounceState = HashMap<(String, u32), Emitted>;

/// What was last emitted for one run: the debounce key value (event + body
/// less `last_seen_at`), when, and the `last_seen_at` it carried — the inputs
/// of the liveness cadence ([`LIVENESS_MERGE_EVERY_MS`]).
#[derive(Debug, Clone, PartialEq)]
struct Emitted {
    event: RunEvent,
    material: JsonValue,
    at_ms: i64,
    last_seen_ms: Option<i64>,
}

/// How often an open, counted run re-reports `last_seen_at` while it stays
/// alive. Coord's backstop closes a run `inferred_lost` after 6 h without a
/// refresh, and the debounce suppresses an otherwise-unchanged body, so a long
/// healthy run needs this explicit liveness `merge` — at most one per run per
/// cadence, driven by the existing liveness poll (never a loop of its own).
pub const LIVENESS_MERGE_EVERY_MS: i64 = 30 * 60 * 1000;

/// Bound on the debounce map; past it the map is cleared (a re-emit is an
/// idempotent upsert, so forgetting is safe).
const DEBOUNCE_CAP: usize = 4096;

/// Emits debounced `session_run` rows into the session outbox. Attached to the
/// [`SessionLifecycleStore`] via `attach_run_record_emitter`, and to the
/// agentic registrar via `AiCoordRegistrar::attach_run_record_emitter`.
pub struct RunRecordEmitter {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    gate: ConsentGate,
    resolver: CoordSessionResolver,
    ledger: AgenticRunLedger,
    last_emitted: Mutex<DebounceState>,
    /// Per run: when a liveness merge was last ATTEMPTED (emitted or not).
    liveness_attempts: Mutex<HashMap<(String, u32), i64>>,
    skipped: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for RunRecordEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRecordEmitter")
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

/// Where the production agentic ledger lives: beside the session outbox.
pub fn agentic_ledger_path() -> PathBuf {
    crate::instance::scope_path(&qontinui_runner_lib::ambient::runner_dir_or_cwd())
        .join("agentic-session-runs.json")
}

impl RunRecordEmitter {
    /// Production constructor: gate 1 reads the real
    /// `Settings.session_metadata_sync_enabled`; the agentic ledger is durable
    /// at [`agentic_ledger_path`]. The outbox MUST be the `Arc` the `CoordSync`
    /// drain reads.
    pub fn new(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        resolver: CoordSessionResolver,
    ) -> Self {
        Self::with_parts(
            outbox,
            machine_id,
            resolver,
            Box::new(crate::settings::get_session_metadata_sync_enabled),
            AgenticRunLedger::open(Some(agentic_ledger_path())),
        )
    }

    /// Constructor with every seam injectable (tests).
    pub fn with_parts(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        resolver: CoordSessionResolver,
        gate: ConsentGate,
        ledger: AgenticRunLedger,
    ) -> Self {
        Self {
            outbox,
            machine_id,
            gate,
            resolver,
            ledger,
            last_emitted: Mutex::new(HashMap::new()),
            liveness_attempts: Mutex::new(HashMap::new()),
            skipped: Mutex::new(HashSet::new()),
        }
    }

    /// Emit `event` for `rec`'s current run. A record with no counted run
    /// emits nothing. Never fails the caller.
    pub fn emit(&self, rec: &TerminalSessionRecord, event: RunEvent) {
        if !(self.gate)() {
            return;
        }
        let coord_session = (self.resolver)(&rec.terminal_id);
        let Some(snap) = RunSnapshot::from_record(rec, coord_session) else {
            return;
        };
        self.emit_snapshot(&snap, event);
    }

    /// Agentic plane: a session registered with coord through the registrar
    /// began a run. `seq` comes from the durable [`AgenticRunLedger`].
    pub fn agentic_start(
        &self,
        claude_session_id: &str,
        spawn: Option<SpawnFacts>,
        tenant_id: Option<String>,
        coord_session_id: Option<Uuid>,
    ) {
        if !(self.gate)() {
            return;
        }
        let now = Utc::now().timestamp_millis();
        let (ended, run) =
            self.ledger
                .open_run(claude_session_id, now, process_boot_ms(), spawn, tenant_id);
        if let Some(prev) = ended {
            self.emit_snapshot(
                &agentic_snapshot(claude_session_id, &prev, None),
                RunEvent::End,
            );
        }
        self.emit_snapshot(
            &agentic_snapshot(claude_session_id, &run, coord_session_id),
            RunEvent::Start,
        );
    }

    /// Whether `snap`'s run is due a liveness merge at `now_ms`: its
    /// `last_seen_at` advanced past what was last emitted for it, and the last
    /// emission is at least [`LIVENESS_MERGE_EVERY_MS`] old. A run with no
    /// emission recorded in this process (a restart) is due once.
    fn liveness_due(&self, snap: &RunSnapshot, now_ms: i64) -> bool {
        let last = self
            .last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (snap.claude_session_id.clone(), snap.run_seq);
        // A liveness ATTEMPT is recorded whether or not it emitted (a body that
        // cannot be built, an outbox append that failed), so a run that cannot
        // emit is retried at the cadence, never on every poll tick.
        let attempts = self
            .liveness_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if attempts
            .get(&key)
            .is_some_and(|at| now_ms - at < LIVENESS_MERGE_EVERY_MS)
        {
            return false;
        }
        match last.get(&key) {
            None => true,
            Some(e) => {
                now_ms - e.at_ms >= LIVENESS_MERGE_EVERY_MS && snap.last_seen_at_ms > e.last_seen_ms
            }
        }
    }

    /// Record a liveness attempt for `snap`'s run at `now_ms` (see
    /// [`Self::liveness_due`]).
    fn note_liveness_attempt(&self, snap: &RunSnapshot, now_ms: i64) {
        let mut attempts = self
            .liveness_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if attempts.len() >= DEBOUNCE_CAP {
            attempts.clear();
        }
        attempts.insert((snap.claude_session_id.clone(), snap.run_seq), now_ms);
    }

    /// Terminal plane: the liveness poll saw `rec` alive (`touch`). Emits one
    /// `merge` carrying the new `last_seen_at` when the cadence is due — only
    /// for a counted, started, not-ended run.
    pub fn liveness(&self, rec: &TerminalSessionRecord) {
        self.liveness_at(rec, Utc::now().timestamp_millis());
    }

    fn liveness_at(&self, rec: &TerminalSessionRecord, now_ms: i64) {
        if !(self.gate)() || !rec.run.start_emitted || rec.run.ended || rec.state != "open" {
            return;
        }
        let coord_session = (self.resolver)(&rec.terminal_id);
        let Some(snap) = RunSnapshot::from_record(rec, coord_session) else {
            return;
        };
        if self.liveness_due(&snap, now_ms) {
            self.note_liveness_attempt(&snap, now_ms);
            self.emit_snapshot_at(&snap, RunEvent::Merge, true, now_ms);
        }
    }

    /// Agentic plane, from the liveness poll's tick: every run THIS process
    /// opened and has not closed is alive by construction (its end is the
    /// process exit, which closes it), so its last activity is now — and a
    /// liveness `merge` goes out when the cadence is due.
    pub fn agentic_liveness_tick(&self) {
        self.agentic_liveness_tick_at(Utc::now().timestamp_millis());
    }

    fn agentic_liveness_tick_at(&self, now_ms: i64) {
        if !(self.gate)() {
            return;
        }
        let mut due = Vec::new();
        for (ccsid, mut run) in self.ledger.current_open_runs(process_boot_ms()) {
            run.last_seen_at = now_ms;
            let snap = agentic_snapshot(&ccsid, &run, None);
            if self.liveness_due(&snap, now_ms) {
                due.push((ccsid, snap));
            }
        }
        if due.is_empty() {
            return;
        }
        // ONE ledger write for the whole tick, before the rows go out.
        self.ledger
            .touch_many(due.iter().map(|(c, _)| c.as_str()), now_ms);
        for (_, snap) in due {
            self.note_liveness_attempt(&snap, now_ms);
            self.emit_snapshot_at(&snap, RunEvent::Merge, true, now_ms);
        }
    }

    /// Agentic plane: an operator interaction with `claude_session_id` — its
    /// open run's last known activity.
    pub fn agentic_touch(&self, claude_session_id: &str) {
        self.ledger
            .touch(claude_session_id, Utc::now().timestamp_millis());
    }

    /// Boot sweep: end every agentic run a previous process left open. Called
    /// once at startup, after the emitter is installed.
    pub fn end_previous_process_agentic_runs(&self) {
        if !(self.gate)() {
            return;
        }
        for (ccsid, run) in self.ledger.end_previous_process_runs(process_boot_ms()) {
            self.emit_snapshot(&agentic_snapshot(&ccsid, &run, None), RunEvent::End);
        }
    }

    /// Agentic plane: the registrar closed the session. No-op when the ledger
    /// holds no open run for it.
    pub fn agentic_end(&self, claude_session_id: &str, coord_session_id: Option<Uuid>) {
        if !(self.gate)() {
            return;
        }
        let now = Utc::now().timestamp_millis();
        let Some(run) = self.ledger.close_run(claude_session_id, now) else {
            return;
        };
        self.emit_snapshot(
            &agentic_snapshot(claude_session_id, &run, coord_session_id),
            RunEvent::End,
        );
    }

    fn emit_snapshot(&self, snap: &RunSnapshot, event: RunEvent) {
        self.emit_snapshot_at(snap, event, false, Utc::now().timestamp_millis());
    }

    /// [`Self::emit_snapshot`] with the clock injected; `liveness` bypasses the
    /// body debounce (the liveness merge's body differs only in `last_seen_at`,
    /// which the debounce deliberately ignores).
    fn emit_snapshot_at(&self, snap: &RunSnapshot, event: RunEvent, liveness: bool, now_ms: i64) {
        let Some(body) = run_record_body(snap, event) else {
            let mut skipped = self
                .skipped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if skipped.insert(snap.claude_session_id.clone()) {
                tracing::debug!(
                    session = %snap.claude_session_id,
                    event = event.as_wire(),
                    "run_record_emitter: session id is not a uuid (or a timestamp is out of range) — run not mirrored"
                );
            }
            return;
        };
        let mut material = body.clone();
        if let Some(o) = material.as_object_mut() {
            o.remove("last_seen_at");
        }
        let key = (snap.claude_session_id.clone(), snap.run_seq);
        let mut last = self
            .last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !liveness
            && last
                .get(&key)
                .is_some_and(|e| e.event == event && e.material == material)
        {
            return;
        }
        let mut payload = json!({ "body": body });
        if let Some(t) = snap.tenant_id.as_deref().filter(|t| !t.is_empty()) {
            payload["tenant_id"] = json!(t);
        }
        match self.outbox.record(
            self.machine_id,
            run_lane(&snap.claude_session_id),
            SessionEventKind::SessionRun,
            payload,
        ) {
            Ok(_) => {
                if last.len() >= DEBOUNCE_CAP {
                    last.clear();
                }
                last.insert(
                    key,
                    Emitted {
                        event,
                        material,
                        at_ms: now_ms,
                        last_seen_ms: snap.last_seen_at_ms,
                    },
                );
            }
            Err(e) => tracing::warn!(
                session = %snap.claude_session_id,
                run_seq = snap.run_seq,
                event = event.as_wire(),
                error = %e,
                "run_record_emitter: outbox append failed (best-effort) — run event dropped locally"
            ),
        }
    }
}

/// The process-wide emitter, installed once at startup (`main.rs`) — the door
/// the agentic plane's process-exit hook reaches it through, since a
/// `ClaudeSession`'s waiter thread holds no handle to it.
static GLOBAL: std::sync::OnceLock<Arc<RunRecordEmitter>> = std::sync::OnceLock::new();

/// Install the process-wide emitter (once).
pub fn install_global(emitter: Arc<RunRecordEmitter>) {
    if GLOBAL.set(emitter).is_err() {
        tracing::warn!("run_record_emitter: global emitter already installed — ignoring");
    }
}

/// The liveness poll's tick for the agentic plane — see
/// [`RunRecordEmitter::agentic_liveness_tick`].
pub fn agentic_liveness_tick() {
    if let Some(emitter) = GLOBAL.get() {
        emitter.agentic_liveness_tick();
    }
}

/// An agentic-plane `ClaudeSession` keyed `session_id` exited — the run's
/// OBSERVED end. A no-op for an id the agentic ledger holds no open run for
/// (every session that was not registered as a run: a CLI-generated id, a
/// terminal session), and for a second exit of the same run.
pub fn agentic_process_exited(session_id: &str) {
    if let Some(emitter) = GLOBAL.get() {
        emitter.agentic_end(session_id, None);
    }
}

/// A HEADLESS child pinned to `session_id` started — a session with neither a
/// lifecycle record nor a registrar registration, so its run rides the agentic
/// ledger directly. Its exit is [`agentic_process_exited`].
pub fn agentic_headless_start(session_id: &str, spawn: SpawnFacts, tenant_id: Option<String>) {
    if let Some(emitter) = GLOBAL.get() {
        emitter.agentic_start(session_id, Some(spawn), tenant_id, None);
    }
}

/// The tenant a headless launch in `workdir` is owned by, as the registrar
/// would resolve it (workspace + repo, plan 2026-09-20 Phase 5) — carried on its
/// run rows so the drain presents that tenant's credential. `None` when no
/// tenant can be stated (the drain's device default then applies).
pub async fn headless_run_tenant(workdir: &str) -> Option<String> {
    super::resolve_new_session_tenant_in(Some(std::path::Path::new(workdir)))
        .await
        .tenant()
        .map(|t| t.to_string())
}

fn agentic_snapshot(ccsid: &str, run: &AgenticRun, coord_session_id: Option<Uuid>) -> RunSnapshot {
    RunSnapshot {
        claude_session_id: ccsid.to_string(),
        run_seq: run.seq,
        started_at_ms: run.opened_at,
        ended_at_ms: run.closed_at,
        close_reason: run.close_reason.clone(),
        last_seen_at_ms: Some(run.last_seen_at.max(run.opened_at)),
        spawn: run.spawn.clone(),
        first_command: None,
        // The agentic plane pins `--session-id` to its task_run_id, so it
        // KNOWS the id exactly.
        bind_origin: Some(ORIGIN_AUTHORITATIVE),
        account_label: None,
        provider: Some(super::session_lifecycle_store::DEFAULT_PROVIDER.to_string()),
        coord_session_id,
        tenant_id: run.tenant_id.clone(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session::session_lifecycle_store::DEFAULT_PROVIDER;
    use tempfile::tempdir;

    pub(crate) const SID: &str = "0b0e8d2c-7a51-4b8e-9c9a-3f1f6a5e2d10";

    pub(crate) fn rec(id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: None,
            working_dir: Some("/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: "term-1".to_string(),
            opened_at: 1_700_000_000_000,
            last_seen_at: 1_700_000_060_000,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: DEFAULT_PROVIDER.to_string(),
            origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: Some(1_700_000_001_000),
            handle: None,
            account_label: Some("gmail".to_string()),
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
            spawn: None,
            run: RunAccounting::default(),
        }
    }

    pub(crate) fn emitter(
        gate_open: bool,
    ) -> (RunRecordEmitter, Arc<OutboxWriter>, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let em = RunRecordEmitter::with_parts(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(|_t: &str| None),
            Box::new(move || gate_open),
            AgenticRunLedger::open(Some(dir.path().join("agentic.json"))),
        );
        (em, outbox, dir)
    }

    fn bodies(outbox: &OutboxWriter) -> Vec<JsonValue> {
        outbox
            .pending()
            .unwrap()
            .into_iter()
            .map(|r| {
                assert_eq!(r.event_kind, SESSION_RUN_EVENT);
                r.payload["body"].clone()
            })
            .collect()
    }

    #[test]
    fn start_then_end_carries_the_store_timestamps_and_raw_close_reason() {
        let (em, outbox, _d) = emitter(true);
        let mut r = rec(SID);
        assert!(begin_run_if_due(&mut r));
        r.spawn = Some(
            SpawnFacts::new(SpawnOrigin::GateContinuation)
                .gate_id(Some("g-1".into()))
                .skill(Some("vet-imp".into())),
        );
        em.emit(&r, RunEvent::Start);
        r.state = "closed".to_string();
        r.closed_at = Some(1_700_000_900_000);
        r.close_reason = Some("poll-dead".to_string());
        em.emit(&r, RunEvent::End);

        let b = bodies(&outbox);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0]["event"], "start");
        assert_eq!(b[0]["run_seq"], 1);
        assert_eq!(b[0]["claude_code_session_id"], SID);
        assert_eq!(b[0]["started_at"], "2023-11-14T22:13:20.000Z");
        assert!(b[0].get("ended_at").is_none());
        assert_eq!(b[0]["facts"]["spawn_origin"], "gate_continuation");
        assert_eq!(b[0]["facts"]["gate_id"], "g-1");
        assert_eq!(b[0]["facts"]["skill"], "vet-imp");
        assert_eq!(b[0]["facts"]["bind_origin"], "authoritative");
        assert_eq!(b[0]["facts"]["account_label"], "gmail");
        assert_eq!(b[1]["event"], "end");
        assert_eq!(b[1]["ended_at"], "2023-11-14T22:28:20.000Z");
        assert_eq!(
            b[1]["close_reason"], "poll-dead",
            "raw close_reason passes through"
        );
        assert_eq!(b[1]["started_at"], b[0]["started_at"]);
        // The row's lane is the dedicated run lane, never a coord session id.
        assert_eq!(outbox.pending().unwrap()[0].session_id, run_lane(SID));
    }

    #[test]
    fn reopen_increments_run_seq_and_uses_the_reopen_time() {
        let mut r = rec(SID);
        assert!(begin_run_if_due(&mut r));
        assert!(
            !begin_run_if_due(&mut r),
            "a counted interval is not re-counted"
        );
        r.state = "closed".to_string();
        r.closed_at = Some(1_700_000_500_000);
        // The store re-opens the closed record.
        r.state = "open".to_string();
        r.closed_at = None;
        r.run.note_opened(1_700_001_000_000);
        assert!(begin_run_if_due(&mut r));
        assert_eq!(r.run.seq, 2);
        let snap = RunSnapshot::from_record(&r, None).unwrap();
        assert_eq!(snap.started_at_ms, 1_700_001_000_000);
    }

    #[test]
    fn an_unconfirmed_record_is_not_a_run() {
        let mut r = rec(SID);
        r.confirmed_at = None;
        assert!(!begin_run_if_due(&mut r));
        assert!(RunSnapshot::from_record(&r, None).is_none());
        let (em, outbox, _d) = emitter(true);
        em.emit(&r, RunEvent::Start);
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn debounces_an_identical_event_but_not_a_new_fact() {
        let (em, outbox, _d) = emitter(true);
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        em.emit(&r, RunEvent::Start);
        r.last_seen_at += 5_000; // liveness churn is not material
        em.emit(&r, RunEvent::Start);
        assert_eq!(bodies(&outbox).len(), 1, "identical start debounced");
        r.run.first_command = Some("vet-imp".to_string());
        em.emit(&r, RunEvent::Merge);
        let b = bodies(&outbox);
        assert_eq!(b.len(), 2, "a learned fact re-emits");
        assert_eq!(b[1]["event"], "merge");
        assert_eq!(b[1]["facts"]["first_command"], "vet-imp");
    }

    #[test]
    fn gate_off_writes_nothing_on_either_plane() {
        let (em, outbox, _d) = emitter(false);
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        em.emit(&r, RunEvent::Start);
        em.agentic_start(SID, None, None, None);
        em.agentic_end(SID, None);
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn a_non_uuid_session_id_is_skipped() {
        let (em, outbox, _d) = emitter(true);
        let mut r = rec("not-a-uuid");
        begin_run_if_due(&mut r);
        em.emit(&r, RunEvent::Start);
        assert!(outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn an_end_with_no_close_time_is_not_sent() {
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        let snap = RunSnapshot::from_record(&r, None).unwrap();
        assert!(run_record_body(&snap, RunEvent::End).is_none());
    }

    #[test]
    fn observed_binds_read_as_authoritative_and_absent_as_reconciled() {
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        r.origin = Some(ORIGIN_OBSERVED.to_string());
        assert_eq!(
            RunSnapshot::from_record(&r, None).unwrap().bind_origin,
            Some(ORIGIN_AUTHORITATIVE)
        );
        r.origin = None;
        assert_eq!(
            RunSnapshot::from_record(&r, None).unwrap().bind_origin,
            Some(ORIGIN_RECONCILED)
        );
    }

    #[test]
    fn agentic_runs_count_durably_across_a_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("agentic.json");
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        // One device across the "restart", as in production.
        let machine = Uuid::new_v4();
        let mk = || {
            RunRecordEmitter::with_parts(
                outbox.clone(),
                machine,
                Box::new(|_t: &str| None),
                Box::new(|| true),
                AgenticRunLedger::open(Some(path.clone())),
            )
        };
        let em = mk();
        em.agentic_start(
            SID,
            Some(SpawnFacts::new(SpawnOrigin::Orchestration)),
            None,
            None,
        );
        em.agentic_end(SID, None);
        em.agentic_end(SID, None); // no open run — no second end
        drop(em);
        // A restarted runner re-registers the resumed session.
        let em = mk();
        em.agentic_start(SID, None, None, None);
        assert_eq!(em.ledger.get(SID).unwrap().seq, 2);

        let b = bodies(&outbox);
        assert_eq!(b.len(), 3);
        assert_eq!(
            (b[0]["event"].clone(), b[0]["run_seq"].clone()),
            (json!("start"), json!(1))
        );
        assert_eq!(b[0]["facts"]["spawn_origin"], "orchestration");
        assert_eq!(
            (b[1]["event"].clone(), b[1]["run_seq"].clone()),
            (json!("end"), json!(1))
        );
        assert_eq!(b[1]["close_reason"], AGENTIC_CLOSE_REASON);
        assert_eq!(
            (b[2]["event"].clone(), b[2]["run_seq"].clone()),
            (json!("start"), json!(2))
        );
    }

    #[test]
    fn an_agentic_run_a_previous_process_left_open_ends_at_its_last_activity() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("agentic.json");
        let mut m = HashMap::new();
        m.insert(
            SID.to_string(),
            AgenticRun {
                seq: 1,
                opened_at: 1_700_000_000_000,
                last_seen_at: 1_700_000_060_000,
                closed_at: None,
                close_reason: None,
                spawn: Some(SpawnFacts::new(SpawnOrigin::OperatorChat)),
                tenant_id: None,
            },
        );
        let other = "1b0e8d2c-7a51-4b8e-9c9a-3f1f6a5e2d11";
        m.insert(
            other.to_string(),
            AgenticRun {
                seq: 4,
                opened_at: 1_700_000_000_000,
                last_seen_at: 0,
                closed_at: None,
                close_reason: None,
                spawn: None,
                tenant_id: None,
            },
        );
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        let em = RunRecordEmitter::with_parts(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(|_t: &str| None),
            Box::new(|| true),
            AgenticRunLedger::open(Some(path.clone())),
        );
        // A re-registration ends the stale run first, then starts run 2.
        em.agentic_start(SID, None, None, None);
        let b = bodies(&outbox);
        assert_eq!(b.len(), 2);
        assert_eq!(
            (b[0]["event"].clone(), b[0]["run_seq"].clone()),
            (json!("end"), json!(1))
        );
        assert_eq!(b[0]["close_reason"], RUNNER_STOPPED_CLOSE_REASON);
        assert_eq!(
            b[0]["ended_at"], "2023-11-14T22:14:20.000Z",
            "its last activity, not now"
        );
        assert_eq!(
            (b[1]["event"].clone(), b[1]["run_seq"].clone()),
            (json!("start"), json!(2))
        );
        assert_eq!(
            b[1]["facts"]["spawn_origin"], "operator_chat",
            "facts carry over"
        );
        // The boot sweep ends every other open run a previous process left —
        // at its opening when no activity was ever recorded — and only once.
        em.end_previous_process_agentic_runs();
        em.end_previous_process_agentic_runs();
        let b = bodies(&outbox);
        assert_eq!(b.len(), 3);
        // Rows on different session lanes have no cross-lane order.
        let swept: Vec<&JsonValue> = b
            .iter()
            .filter(|x| x["claude_code_session_id"] == other)
            .collect();
        assert_eq!(swept.len(), 1, "the sweep ends it exactly once");
        assert_eq!(
            (swept[0]["event"].clone(), swept[0]["run_seq"].clone()),
            (json!("end"), json!(4))
        );
        assert_eq!(swept[0]["ended_at"], "2023-11-14T22:13:20.000Z");
        // The current run (this process's) is untouched by the sweep.
        assert!(em.ledger.get(SID).unwrap().closed_at.is_none());
    }

    #[test]
    fn a_long_run_refreshes_last_seen_once_per_cadence() {
        let (em, outbox, _d) = emitter(true);
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        r.spawn = Some(SpawnFacts::new(SpawnOrigin::Steward));
        assert!(claim_start(&mut r, false));
        let t0 = Utc::now().timestamp_millis();
        em.emit_snapshot_at(
            &RunSnapshot::from_record(&r, None).unwrap(),
            RunEvent::Start,
            false,
            t0,
        );
        // Touched UNDER the cadence: no merge, however often.
        r.last_seen_at += 60_000;
        em.liveness_at(&r, t0 + 10 * 60 * 1000);
        em.liveness_at(&r, t0 + LIVENESS_MERGE_EVERY_MS - 1);
        assert_eq!(bodies(&outbox).len(), 1, "under the cadence emits none");
        // Touched PAST it: exactly one merge carrying the new last_seen_at.
        r.last_seen_at = 1_700_009_000_000;
        em.liveness_at(&r, t0 + LIVENESS_MERGE_EVERY_MS);
        em.liveness_at(&r, t0 + LIVENESS_MERGE_EVERY_MS + 1_000);
        let b = bodies(&outbox);
        assert_eq!(b.len(), 2, "one merge per cadence");
        assert_eq!(b[1]["event"], "merge");
        assert_eq!(b[1]["last_seen_at"], "2023-11-15T00:43:20.000Z");
        // A past cadence with NO new sighting refreshes nothing.
        em.liveness_at(&r, t0 + 3 * LIVENESS_MERGE_EVERY_MS);
        assert_eq!(bodies(&outbox).len(), 2, "no advance, no merge");
        // An ended run is never refreshed.
        r.run.ended = true;
        r.last_seen_at += 1;
        em.liveness_at(&r, t0 + 5 * LIVENESS_MERGE_EVERY_MS);
        assert_eq!(bodies(&outbox).len(), 2);
    }

    #[test]
    fn a_store_touch_under_the_cadence_emits_no_liveness_merge() {
        let (store, outbox, _d) = store_with_emitter();
        store.record_open(rec(SID));
        store.touch(SID); // releases the held start
        store.touch(SID);
        store.touch(SID);
        assert_eq!(events(&outbox), vec![("start".to_string(), 1)]);
    }

    #[test]
    fn an_open_agentic_run_refreshes_last_seen_once_per_cadence() {
        let (em, outbox, _d) = emitter(true);
        em.agentic_start(
            SID,
            Some(SpawnFacts::new(SpawnOrigin::Scheduler)),
            None,
            None,
        );
        let t0 = Utc::now().timestamp_millis();
        em.agentic_liveness_tick_at(t0 + 60_000);
        assert_eq!(bodies(&outbox).len(), 1, "under the cadence emits none");
        let later = t0 + LIVENESS_MERGE_EVERY_MS + 60_000;
        em.agentic_liveness_tick_at(later);
        em.agentic_liveness_tick_at(later + 1_000);
        let b = bodies(&outbox);
        assert_eq!(b.len(), 2, "one merge per cadence");
        assert_eq!(b[1]["event"], "merge");
        assert_eq!(b[1]["run_seq"], 1);
        assert_eq!(b[1]["last_seen_at"], rfc3339(later).unwrap());
        assert_eq!(em.ledger.get(SID).unwrap().last_seen_at, later, "persisted");
        // A closed run is never refreshed.
        em.agentic_end(SID, None);
        em.agentic_liveness_tick_at(later + 2 * LIVENESS_MERGE_EVERY_MS);
        assert_eq!(bodies(&outbox).len(), 3, "start, merge, end — no more");
    }

    #[test]
    fn a_run_that_cannot_emit_retries_liveness_at_the_cadence_not_every_tick() {
        let (em, outbox, _d) = emitter(true);
        // A non-uuid id: its body can never be built, so nothing is recorded
        // as emitted for it.
        let mut r = rec("not-a-uuid");
        begin_run_if_due(&mut r);
        r.run.start_emitted = true;
        let t0 = Utc::now().timestamp_millis();
        let snap = RunSnapshot::from_record(&r, None).unwrap();
        assert!(em.liveness_due(&snap, t0), "due once (nothing recorded)");
        em.liveness_at(&r, t0);
        assert!(outbox.pending().unwrap().is_empty());
        assert!(
            !em.liveness_due(&snap, t0 + 45_000),
            "not again on the next tick"
        );
        assert!(!em.liveness_due(&snap, t0 + LIVENESS_MERGE_EVERY_MS - 1));
        assert!(
            em.liveness_due(&snap, t0 + LIVENESS_MERGE_EVERY_MS),
            "retried at the cadence"
        );
    }

    #[test]
    fn a_failed_tier_keeps_the_runners_facts_only_while_its_restore_is_pending() {
        let boot = process_boot_ms();
        let mk = |pending: bool| {
            let mut r = rec(SID);
            r.run = RunAccounting {
                seq: 1,
                opened_at: Some(boot - 1),
                started: true,
                ended: true,
                start_emitted: true,
                first_command: None,
            };
            r.restored_from_boot_at = Some(boot + 1);
            r.restore_tier =
                Some(crate::session::session_lifecycle_store::RESTORE_TIER_FAILED.to_string());
            r.restore_pending_at = pending.then_some(boot + 1);
            r.spawn =
                Some(SpawnFacts::new(SpawnOrigin::GateContinuation).gate_id(Some("g".into())));
            r
        };
        let mut pending = mk(true);
        assert!(begin_next_interval(&mut pending, boot + 2, boot));
        assert_eq!(
            pending.spawn.as_ref().map(|f| f.spawn_origin),
            Some(SpawnOrigin::BootResume)
        );
        assert_eq!(pending.spawn.unwrap().gate_id.as_deref(), Some("g"));
        let mut settled = mk(false);
        assert!(begin_next_interval(&mut settled, boot + 2, boot));
        assert!(
            settled.spawn.is_none(),
            "a failed restore no longer pending: fresh facts"
        );
    }

    #[test]
    fn a_close_reason_outside_the_wire_charset_becomes_other() {
        assert_eq!(wire_close_reason("pty-exit"), "pty-exit");
        assert_eq!(
            wire_close_reason("superseded-terminal-reuse"),
            "superseded-terminal-reuse"
        );
        assert_eq!(wire_close_reason("a:b.c_d-1"), "a:b.c_d-1");
        for bad in [
            "orchestration dispatch could not bind the worker",
            "Poll-Dead",
            "",
            &"x".repeat(65),
            "x/y",
        ] {
            assert_eq!(wire_close_reason(bad), "other", "{bad:?}");
        }
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        r.state = "closed".to_string();
        r.closed_at = Some(1_700_000_900_000);
        r.close_reason = Some("orchestration dispatch could not bind the worker".to_string());
        let body =
            run_record_body(&RunSnapshot::from_record(&r, None).unwrap(), RunEvent::End).unwrap();
        assert_eq!(body["close_reason"], "other");
        assert!(!body.to_string().contains("orchestration dispatch"));
    }

    /// The body in coord's strict contract shape: every id a REAL uuid, every
    /// timestamp RFC 3339, `run_seq` a positive integer, only the documented
    /// keys at the top and in `facts`.
    #[test]
    fn the_wire_body_matches_coords_strict_contract() {
        let gate = Uuid::new_v4();
        let agent = Uuid::new_v4();
        let coord_session = Uuid::new_v4();
        let mut r = rec(SID);
        begin_run_if_due(&mut r);
        r.spawn = Some(
            SpawnFacts::new(SpawnOrigin::GateContinuation)
                .gate_id(Some(gate.to_string()))
                .agent_id(Some(agent.to_string()))
                .skill(Some("vet-imp".into())),
        );
        r.state = "closed".to_string();
        r.closed_at = Some(1_700_000_900_000);
        r.close_reason = Some("pty-exit".to_string());
        let body = run_record_body(
            &RunSnapshot::from_record(&r, Some(coord_session)).unwrap(),
            RunEvent::End,
        )
        .unwrap();
        let top: std::collections::BTreeSet<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let allowed_top = [
            "claude_code_session_id",
            "run_seq",
            "event",
            "started_at",
            "ended_at",
            "close_reason",
            "last_seen_at",
            "first_turn_at",
            "active_seconds",
            "facts",
        ];
        assert!(top.iter().all(|k| allowed_top.contains(k)), "{top:?}");
        let allowed_facts = [
            "spawn_origin",
            "spawn_kind",
            "gate_id",
            "agent_id",
            "coord_session_id",
            "spawn_request_id",
            "skill",
            "first_command",
            "bind_origin",
            "account_label",
            "provider",
            "model",
            "repo",
            "work_unit_slug",
        ];
        let facts = body["facts"].as_object().unwrap();
        assert!(
            facts.keys().all(|k| allowed_facts.contains(&k.as_str())),
            "{facts:?}"
        );
        for key in ["gate_id", "agent_id", "coord_session_id"] {
            Uuid::parse_str(facts[key].as_str().unwrap()).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
        Uuid::parse_str(body["claude_code_session_id"].as_str().unwrap()).unwrap();
        assert!(body["run_seq"].as_u64().unwrap() >= 1);
        for key in ["started_at", "ended_at", "last_seen_at"] {
            DateTime::parse_from_rfc3339(body[key].as_str().unwrap())
                .unwrap_or_else(|e| panic!("{key}: {e}"));
        }
        assert_eq!(body["event"], "end");
        assert_eq!(facts["bind_origin"], "authoritative");
    }

    #[test]
    fn tenant_rides_the_envelope_not_the_body() {
        let (em, outbox, _d) = emitter(true);
        let mut r = rec(SID);
        r.tenant_id = Some("4f8c1c1e-1111-4222-8333-444455556666".to_string());
        begin_run_if_due(&mut r);
        em.emit(&r, RunEvent::Start);
        let row = &outbox.pending().unwrap()[0];
        assert_eq!(
            row.payload["tenant_id"],
            "4f8c1c1e-1111-4222-8333-444455556666"
        );
        assert!(row.payload["body"].get("tenant_id").is_none());
    }

    // -- through the real store ----------------------------------------------

    use crate::session::session_lifecycle_store::SessionLifecycleStore;

    fn store_with_emitter() -> (SessionLifecycleStore, Arc<OutboxWriter>, tempfile::TempDir) {
        let (em, outbox, dir) = emitter(true);
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        store.attach_run_record_emitter(Arc::new(em));
        (store, outbox, dir)
    }

    fn events(outbox: &OutboxWriter) -> Vec<(String, u64)> {
        bodies(outbox)
            .iter()
            .map(|b| {
                (
                    b["event"].as_str().unwrap().to_string(),
                    b["run_seq"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn store_open_confirm_close_reopen_emits_start_end_and_the_next_run() {
        let (store, outbox, _d) = store_with_emitter();
        // A provisional (unconfirmed) record is not a run.
        let mut r = rec(SID);
        r.confirmed_at = None;
        store.record_open(r.clone());
        assert!(
            events(&outbox).is_empty(),
            "a provisional record emits nothing"
        );
        // Confirmation begins run 1 — its start is HELD for the launch facts…
        store.confirm_session(SID);
        assert!(events(&outbox).is_empty(), "start held until the stamp");
        // …which the launcher's stamp releases, carrying them.
        assert!(store.record_spawn_facts(
            "term-1",
            SpawnFacts::new(SpawnOrigin::OperatorTerminal),
            0
        ));
        assert!(store.note_first_command(SID, "/vet-imp"));
        store.record_close(SID, "pty-exit");
        store.record_close(SID, "pty-exit"); // repeat close: no second end
                                             // Re-opening the closed record is run 2: the old facts are dropped,
                                             // and its start waits for THIS launch's stamp.
        store.record_open(rec(SID));
        assert_eq!(store.get(SID).unwrap().run.seq, 2);
        assert!(
            store.get(SID).unwrap().spawn.is_none(),
            "run 1's facts dropped"
        );
        assert!(store.record_spawn_facts(
            "term-1",
            SpawnFacts::new(SpawnOrigin::Respawn).spawn_kind("resume_door"),
            0
        ));
        assert_eq!(
            events(&outbox),
            vec![
                ("start".to_string(), 1),
                ("merge".to_string(), 1),
                ("end".to_string(), 1),
                ("start".to_string(), 2),
            ]
        );
        let b = bodies(&outbox);
        assert_eq!(b[0]["facts"]["spawn_origin"], "operator_terminal");
        assert_eq!(b[2]["close_reason"], "pty-exit", "raw store close reason");
        assert_eq!(b[2]["facts"]["spawn_origin"], "operator_terminal");
        assert_eq!(b[2]["facts"]["first_command"], "vet-imp");
        assert_eq!(
            b[3]["facts"]["spawn_origin"], "respawn",
            "run 2 carries the NEW facts"
        );
        assert_eq!(b[3]["facts"]["spawn_kind"], "resume_door");
    }

    #[test]
    fn a_run_no_launcher_stamps_is_released_by_the_first_touch() {
        let (store, outbox, _d) = store_with_emitter();
        store.record_open(rec(SID)); // confirmed, no facts: held
        assert!(events(&outbox).is_empty());
        store.touch(SID);
        store.touch(SID);
        assert_eq!(events(&outbox), vec![("start".to_string(), 1)]);
        assert!(bodies(&outbox)[0]["facts"].get("spawn_origin").is_none());
    }

    #[test]
    fn a_prune_of_a_stale_open_record_reports_its_end_at_its_last_sighting() {
        let (store, outbox, dir) = store_with_emitter();
        let path = dir.path().join("terminal-sessions.json");
        store.record_open(rec(SID));
        store.touch(SID); // release the held start
        store.compact();
        let raw = std::fs::read(&path).unwrap();
        let mut m: HashMap<String, TerminalSessionRecord> = serde_json::from_slice(&raw).unwrap();
        m.get_mut(SID).unwrap().last_seen_at = 1_700_000_060_000;
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        store.reload_from_disk().unwrap();
        store.prune(Utc::now().timestamp_millis(), &HashSet::new());
        assert!(store.get(SID).is_none(), "the stale row is pruned");
        let b = bodies(&outbox);
        let end = b.last().unwrap();
        assert_eq!(end["event"], "end");
        assert_eq!(end["ended_at"], "2023-11-14T22:14:20.000Z");
        assert_eq!(
            end["close_reason"],
            crate::session::session_lifecycle_store::PRUNED_STALE_OPEN_CLOSE_REASON
        );
    }

    #[test]
    fn a_close_of_a_run_a_previous_process_opened_is_clamped_to_its_last_sighting() {
        let (em, outbox, dir) = emitter(true);
        let path = dir.path().join("terminal-sessions.json");
        let mut r = rec(SID);
        r.run = RunAccounting {
            seq: 1,
            opened_at: Some(1_700_000_000_000),
            started: true,
            ended: false,
            start_emitted: true,
            first_command: None,
        };
        let mut m = HashMap::new();
        m.insert(SID.to_string(), r);
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.attach_run_record_emitter(Arc::new(em));
        // The liveness poll finds it dead long after this process booted.
        store.record_close(SID, "poll-dead");
        let b = bodies(&outbox);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["ended_at"], "2023-11-14T22:14:20.000Z", "not now");
        assert_eq!(b[0]["close_reason"], "poll-dead");
    }

    #[test]
    fn run_seq_survives_a_store_reload() {
        let (store, _outbox, dir) = store_with_emitter();
        store.record_open(rec(SID));
        store.record_close(SID, "poll-dead");
        drop(store);
        let reopened =
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let r = reopened.get(SID).unwrap();
        assert_eq!((r.run.seq, r.run.started, r.run.ended), (1, true, true));
        reopened.record_open(rec(SID));
        assert_eq!(reopened.get(SID).unwrap().run.seq, 2);
    }

    #[test]
    fn a_superseded_terminal_reuse_close_ends_the_prior_run() {
        let (store, outbox, _d) = store_with_emitter();
        store.record_open(rec(SID));
        let other = "1b0e8d2c-7a51-4b8e-9c9a-3f1f6a5e2d11";
        store.record_open(rec(other)); // same terminal, confirmed ⇒ retires SID
        let b = bodies(&outbox);
        let end = b
            .iter()
            .find(|x| x["event"] == "end")
            .expect("prior run ended");
        assert_eq!(end["claude_code_session_id"], SID);
        assert_eq!(end["close_reason"], "superseded-terminal-reuse");
    }

    /// A record a PREVIOUS runner process was running, persisted with an open
    /// interval that began long before this process's boot instant.
    fn previous_process_store(
        spawn: Option<SpawnFacts>,
    ) -> (SessionLifecycleStore, Arc<OutboxWriter>, tempfile::TempDir) {
        let (em, outbox, dir) = emitter(true);
        let path = dir.path().join("terminal-sessions.json");
        let mut r = rec(SID);
        r.run = RunAccounting {
            seq: 1,
            opened_at: Some(1_700_000_000_000),
            started: true,
            ended: false,
            start_emitted: true,
            first_command: None,
        };
        r.spawn = spawn;
        let mut m = HashMap::new();
        m.insert(SID.to_string(), r);
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.attach_run_record_emitter(Arc::new(em));
        (store, outbox, dir)
    }

    #[test]
    fn a_boot_restore_ends_the_previous_run_and_starts_the_next_only_once_verified() {
        let (store, outbox, _d) = previous_process_store(Some(
            SpawnFacts::new(SpawnOrigin::GateContinuation).gate_id(Some("g".into())),
        ));
        store.mark_restore_pending(SID);
        store.mark_restore_pending(SID); // a retry this boot is idempotent
        assert_eq!(
            events(&outbox),
            vec![("end".to_string(), 1)],
            "no run 2 before the resume is verified"
        );
        store.mark_restored_from_boot(SID, "resumed");
        // The verified handshake starts run 2.
        store.clear_restore_pending(SID);
        assert_eq!(
            events(&outbox),
            vec![("end".to_string(), 1), ("start".to_string(), 2)]
        );
        let b = bodies(&outbox);
        assert_eq!(b[0]["close_reason"], RUNNER_STOPPED_CLOSE_REASON);
        // Ended at the last moment the previous process saw it alive.
        assert_eq!(b[0]["ended_at"], "2023-11-14T22:14:20.000Z");
        assert_eq!(b[1]["facts"]["spawn_origin"], "boot_resume");
        assert_eq!(b[1]["facts"]["gate_id"], "g", "the work's facts carry over");
    }

    #[test]
    fn a_resume_typed_into_a_terminal_only_tile_is_the_next_run() {
        let (store, outbox, _d) = previous_process_store(Some(
            SpawnFacts::new(SpawnOrigin::GateContinuation).gate_id(Some("old-gate".into())),
        ));
        store.mark_restored_from_boot(SID, "terminal-only");
        // The frontend re-asserts the tile (unconfirmed): not a run.
        let mut reassert = rec(SID);
        reassert.confirmed_at = None;
        store.record_open(reassert);
        assert_eq!(events(&outbox), vec![("end".to_string(), 1)]);
        // The operator types `claude --resume` — its hook confirms the id.
        store.record_open(rec(SID));
        // No launcher stamps a typed resume: the first liveness touch releases
        // its held start (with fresh, empty facts).
        store.touch(SID);
        store.record_close(SID, "pty-exit");
        store.record_close(SID, "pty-exit");
        assert_eq!(
            events(&outbox),
            vec![
                ("end".to_string(), 1),
                ("start".to_string(), 2),
                ("end".to_string(), 2),
            ]
        );
        // The typed resume is a NEW launch: run 1's facts do not ride run 2.
        let b = bodies(&outbox);
        assert_eq!(b[0]["facts"]["gate_id"], "old-gate");
        assert!(b[1]["facts"].get("gate_id").is_none(), "{}", b[1]);
        assert!(b[1]["facts"].get("spawn_origin").is_none(), "{}", b[1]);
    }

    #[test]
    fn spawn_facts_do_not_move_onto_a_record_this_spawn_did_not_open() {
        let (store, _outbox, _d) = store_with_emitter();
        store.record_open(rec(SID));
        let later = Utc::now().timestamp_millis() + 60_000;
        assert!(!store.record_spawn_facts("term-1", SpawnFacts::new(SpawnOrigin::Respawn), later));
        assert!(store.get(SID).unwrap().spawn.is_none());
    }

    #[test]
    fn a_new_record_on_the_terminal_inherits_the_seam_rows_facts() {
        let (store, _outbox, _d) = store_with_emitter();
        let mut seam = rec("2b0e8d2c-7a51-4b8e-9c9a-3f1f6a5e2d12");
        seam.confirmed_at = None;
        seam.origin = None;
        store.record_open(seam);
        assert!(store.record_spawn_facts("term-1", SpawnFacts::new(SpawnOrigin::Steward), 0));
        store.record_open(rec(SID)); // the launcher's own typed row
        assert_eq!(
            store.get(SID).unwrap().spawn.map(|f| f.spawn_origin),
            Some(SpawnOrigin::Steward)
        );
    }

    #[test]
    fn skill_of_prompt_reads_only_a_leading_slash_command() {
        assert_eq!(
            skill_of_prompt("/vet-imp plans/secret.md"),
            Some("vet-imp".to_string())
        );
        assert_eq!(
            skill_of_prompt("  /implement-phase\n3"),
            Some("implement-phase".to_string())
        );
        assert_eq!(skill_of_prompt("run /vet-imp now"), None);
        assert_eq!(skill_of_prompt("/Weird-Case x"), None);
        assert_eq!(skill_of_prompt(""), None);
    }

    #[test]
    fn first_command_validation_keeps_the_name_and_drops_everything_else() {
        assert_eq!(valid_first_command("/vet-imp"), Some("vet-imp".to_string()));
        assert_eq!(
            valid_first_command("plugin:skill_x-1"),
            Some("plugin:skill_x-1".to_string())
        );
        for bad in [
            "",
            "/",
            "/Vet",
            "/vet imp",
            "vet-imp plan.md",
            "/a/b",
            "x\n",
            &"a".repeat(65),
        ] {
            assert_eq!(valid_first_command(bad), None, "{bad:?}");
        }
        assert_eq!(
            valid_first_command(&"a".repeat(64)).map(|s| s.len()),
            Some(64)
        );
    }
}
