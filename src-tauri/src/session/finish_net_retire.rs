//! Retire a session's implement→ship safety nets BEFORE a runner finish door
//! writes its finished mark (plan
//! `2026-11-23-an-operator-finish-from-the-runner-ui-still-fires-the-finished-sessions-implement-ship-net`,
//! Phase 1).
//!
//! **Why.** `/implement-plan` Step 0.5 arms a `claim_terminal` gate on the
//! session's `plan:<stem>` reserve whose continuation dispatches a fresh
//! `/implement-plan` when the reserve goes away. A finished session's reserve
//! is released later — by the claim heartbeat on `coord_closed_idle`, or within
//! one heartbeat slice when its pane closes (`owner_gone`) — and coord cannot
//! tell that release from a death, so the net clears and dispatches a duplicate
//! onto work that was just declared done. The finish COMMANDS retire the net
//! through `scripts/retire-nets-for-finish.sh`; the runner's own finish doors
//! (the Tauri `terminal_session_set_finished`, `POST /sessions/{id}/finish`, and
//! the continuation finish relay) are fronted by no command, so they retire it
//! here, through ONE helper.
//!
//! **Contract** — a port of `qontinui-claude-config`
//! `scripts/plan-reserve-release.sh` `retire-net`, as wrapped by
//! `retire-nets-for-finish.sh`:
//!
//! 1. List the target's reserves with the anonymous
//!    `GET /coord/claims/list?kind=semantic_resource&prefix=plan%3A`, keeping
//!    rows whose `session_id` is the finished session (case-insensitive). No
//!    match is `NONE`, unless the page was `truncated` — then `UNKNOWN`.
//! 2. **Tenant rule** (served `coordination`
//!    `repo-writes-go-to-the-owning-tenant`): only when this device is PROVEN
//!    bound to a single tenant — the plan adapter's own
//!    [`work_unit_write_posture`] answer — is the device JWT's tenant the only
//!    possible owner. Otherwise nothing is read or written and every reserve
//!    reads `UNRETIRED (owner tenant unproven)`.
//! 3. Read each plan's gates (`GET /coord/agent-gates?work_unit_slug=<stem>`,
//!    every verdict — that route has no `open_only`). Select
//!    `predicate.kind == claim_terminal` rows whose `phase_name` begins
//!    `implement→ship safety net`; those `registered_by` THIS device are acted
//!    on, any other is `NET_INERT` (named, untouched — coord's withdraw
//!    authorizes on the registering device).
//! 4. Withdraw an open net (`POST /coord/gates/<id>/withdraw`); a
//!    not-the-registrant refusal (403) falls back to cancelling the
//!    continuation (`POST /coord/gates/<id>/agent/continuation-cancel`), and
//!    NEVER to a mute. A cleared net whose continuation is still live gets its
//!    continuation cancelled. Every write is READ BACK.
//!
//! **Never blocks the mark.** Whatever this answers, the door writes the mark;
//! the outcome rides the door's response as `nets` so an unretired net is on
//! the record. The whole run is bounded (the doors pass [`DOOR_BUDGET`]) and runs outside
//! every store lock — never from the finish observer, which holds
//! `finish_forward` across `set_finished_inner`.
//!
//! [`work_unit_write_posture`]: qontinui_runner_lib::plan_workunit_adapter::trigger::work_unit_write_posture

use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{info, warn};

use crate::session::session_lifecycle_store::SessionLifecycleStore;

/// A finish door's whole retire budget. The doors answer callers that bound
/// their own wait (`/finish-session` curls with `-m 15`), so the retire must
/// end well inside that even when coord is dark; past it the outcome is
/// UNKNOWN and the mark is written anyway.
pub const DOOR_BUDGET: Duration = Duration::from_secs(10);
/// A call starts only while at least this much budget is left.
const CALL_FLOOR: Duration = Duration::from_secs(3);
/// Per-call cap for every coord call (a gate read of all its pages counts as
/// one call), applied per request and again by the run's [`Budget`].
const CALL_CAP: Duration = Duration::from_secs(5);
/// Gate pages read per plan before the read is called short (5 × 100).
const MAX_GATE_PAGES: usize = 5;

/// The net's `phase_name` prefix (/implement-plan Step 0.5). A prefix, so a
/// variant name still matches (/vet-imp Step 5's rule).
pub const NET_PHASE_PREFIX: &str = "implement\u{2192}ship safety net";

/// What an unmark answers under `nets.rearm`: a withdrawn net is never
/// re-armed by an unfinish.
pub const REARM_NOTE: &str = "not restored — /implement-plan Step 0.5 re-arms";

// ===========================================================================
// The outcome
// ===========================================================================

/// One-word verdict of a retire run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetsVerdict {
    /// Reserves were found and every net on them is retired or inert.
    Retired,
    /// The session holds no plan reserve.
    None,
    /// A net is (or may be) still armed: withdraw failed, or the owner tenant
    /// is unproven.
    NetUnretired,
    /// A read could not be made (claims list, gate read), so whether a net is
    /// armed is not known.
    Unknown,
}

impl NetsVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            NetsVerdict::Retired => "RETIRED",
            NetsVerdict::None => "NONE",
            NetsVerdict::NetUnretired => "NET_UNRETIRED",
            NetsVerdict::Unknown => "UNKNOWN",
        }
    }
}

/// A net (or a reserve whose nets could not be read) this run did NOT retire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnretiredNet {
    pub stem: String,
    /// `None` when the net itself could not be read or was never looked for.
    pub gate_id: Option<String>,
    pub why: String,
    /// The cause is an unreadable read (→ `UNKNOWN`), not a known armed net
    /// (→ `NET_UNRETIRED`).
    #[serde(skip)]
    pub unreadable: bool,
}

/// A net found and deliberately left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InertNet {
    pub stem: String,
    pub gate_id: String,
    pub why: String,
}

/// What [`retire_nets_for_finish`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetsOutcome {
    /// The `plan:<stem>` reserves the session held.
    pub stems: Vec<String>,
    pub withdrawn: Vec<String>,
    pub cancelled: Vec<String>,
    pub inert: Vec<InertNet>,
    pub unretired: Vec<UnretiredNet>,
    /// Set when the run as a whole is UNKNOWN (an unreadable or truncated
    /// claims list).
    pub unknown: Option<String>,
}

impl NetsOutcome {
    fn unknown(why: impl Into<String>) -> Self {
        NetsOutcome {
            unknown: Some(why.into()),
            ..Default::default()
        }
    }

    pub fn verdict(&self) -> NetsVerdict {
        if self.unretired.iter().any(|u| !u.unreadable) {
            NetsVerdict::NetUnretired
        } else if self.unknown.is_some() || self.unretired.iter().any(|u| u.unreadable) {
            NetsVerdict::Unknown
        } else if self.stems.is_empty() {
            NetsVerdict::None
        } else {
            NetsVerdict::Retired
        }
    }

    /// The door response's `nets` object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": self.verdict().as_str(),
            "stems": self.stems,
            "withdrawn": self.withdrawn,
            "cancelled": self.cancelled,
            "inert": self.inert,
            "unretired": self.unretired,
            "note": self.unknown,
        })
    }

    fn unretire(&mut self, stem: &str, gate_id: Option<&str>, why: impl Into<String>) {
        self.unretired.push(UnretiredNet {
            stem: stem.to_string(),
            gate_id: gate_id.map(str::to_string),
            why: why.into(),
            unreadable: false,
        });
    }

    fn unreadable(&mut self, stem: &str, gate_id: Option<&str>, why: impl Into<String>) {
        self.unretired.push(UnretiredNet {
            stem: stem.to_string(),
            gate_id: gate_id.map(str::to_string),
            why: why.into(),
            unreadable: true,
        });
    }

    fn inert(&mut self, stem: &str, gate_id: &str, why: impl Into<String>) {
        self.inert.push(InertNet {
            stem: stem.to_string(),
            gate_id: gate_id.to_string(),
            why: why.into(),
        });
    }
}

/// What a finish door did about nets, for its response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetsReport {
    /// The helper ran.
    Ran(NetsOutcome),
    /// A finish that writes no mark (an unknown id, or a read-back of a mark
    /// already set): the helper was not run. The `&str` says why.
    Skipped(&'static str),
    /// An unmark: never runs the helper, never re-arms.
    Unmark,
}

impl NetsReport {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            NetsReport::Ran(o) => o.to_json(),
            NetsReport::Skipped(why) => serde_json::json!({ "verdict": null, "skipped": why }),
            NetsReport::Unmark => serde_json::json!({ "rearm": REARM_NOTE }),
        }
    }

    /// Add `nets` to a door's [`FinishOutcome::response_json`] body.
    ///
    /// [`FinishOutcome::response_json`]: crate::session::session_lifecycle_store::FinishOutcome::response_json
    pub fn attach(&self, body: &mut serde_json::Value) {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("nets".to_string(), self.to_json());
        }
    }
}

// ===========================================================================
// The door seam
// ===========================================================================

/// What a finish door calls before it writes the mark. Production is
/// [`CoordNetRetirer`]; tests inject a recording fake to pin the
/// retire-before-mark order at each door.
#[async_trait::async_trait]
pub trait FinishNetRetirer: Send + Sync {
    /// Retire within `budget`; past it the outcome is UNKNOWN.
    async fn retire(&self, claude_session_id: &str, by: &str, budget: Duration) -> NetsOutcome;
}

/// The production retirer: [`retire_nets_for_finish`].
pub struct CoordNetRetirer;

#[async_trait::async_trait]
impl FinishNetRetirer for CoordNetRetirer {
    async fn retire(&self, claude_session_id: &str, by: &str, budget: Duration) -> NetsOutcome {
        retire_nets_for_finish(claude_session_id, by, budget).await
    }
}

/// The step every finish door runs BEFORE its `set_finished` call.
///
/// Runs the retirer only for a finish that will WRITE the mark: never on an
/// unmark (`finished: false`), never for an id the store does not know, and
/// never for a re-finish that writes nothing — a reason-less re-finish of a
/// finished mark (the HTTP read-back), or, with `only_if_unfinished` (the
/// relay's conditional write), any finished mark.
pub async fn retire_before_finish(
    store: &SessionLifecycleStore,
    retirer: &dyn FinishNetRetirer,
    claude_session_id: &str,
    finished: bool,
    reason: Option<&str>,
    only_if_unfinished: bool,
    by: &str,
    budget: Duration,
) -> NetsReport {
    if !finished {
        return NetsReport::Unmark;
    }
    let Some(record) = store.get(claude_session_id) else {
        return NetsReport::Skipped("the session is unknown to the lifecycle registry");
    };
    if record.finished_at.is_some() && (only_if_unfinished || reason.is_none()) {
        return NetsReport::Skipped(
            "the mark is already set and this call writes none; nets are retired when a finish writes the mark",
        );
    }
    NetsReport::Ran(retirer.retire(claude_session_id, by, budget).await)
}

// ===========================================================================
// Coord I/O seam
// ===========================================================================

/// One `plan:` reserve holder row, as the claims list serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveRow {
    pub resource_key: String,
    pub session_id: Option<String>,
}

/// One claims-list page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservePage {
    pub holders: Vec<ReserveRow>,
    pub truncated: bool,
}

/// The fields of one gate row this run reads, from `GET /coord/agent-gates`'
/// flat shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GateRow {
    pub gate_id: String,
    pub verdict: String,
    pub predicate_kind: String,
    pub phase_name: String,
    pub registered_by: String,
    pub will_dispatch: Option<bool>,
    pub consumed: bool,
    pub cancelled: bool,
    /// `continuation_spawn` is present: `Some(spawn != null)`.
    pub armed: Option<bool>,
}

impl GateRow {
    /// Parse one row; `None` when it carries no `gate_id`.
    pub fn from_json(row: &serde_json::Value) -> Option<GateRow> {
        let s = |k: &str| {
            row.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let gate_id = s("gate_id").to_ascii_lowercase();
        if gate_id.is_empty() {
            return None;
        }
        let predicate_kind = row
            .get("predicate_kind")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                row.get("predicate")
                    .and_then(|p| p.get("kind"))
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("")
            .to_string();
        let present = |k: &str| row.get(k).is_some_and(|v| !v.is_null());
        Some(GateRow {
            gate_id,
            verdict: s("verdict"),
            predicate_kind,
            phase_name: s("phase_name"),
            registered_by: s("registered_by").to_ascii_lowercase(),
            will_dispatch: row
                .get("continuation_will_dispatch")
                .and_then(serde_json::Value::as_bool),
            consumed: present("continuation_consumed_at"),
            cancelled: present("continuation_cancelled_at"),
            armed: row.get("continuation_spawn").map(|v| !v.is_null()),
        })
    }

    /// An implement→ship net: `claim_terminal` under the net's phase prefix.
    pub fn is_net(&self) -> bool {
        self.predicate_kind == "claim_terminal" && self.phase_name.starts_with(NET_PHASE_PREFIX)
    }

    /// Can this row's continuation still dispatch? (the script's `cont_live`)
    pub fn continuation_live(&self) -> bool {
        if self.armed == Some(false) || self.will_dispatch == Some(false) {
            return false;
        }
        if self.will_dispatch == Some(true) {
            return true;
        }
        !(self.cancelled || self.consumed)
    }
}

/// A gate write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateWrite {
    Withdraw,
    CancelContinuation,
}

/// What coord answered a gate write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteAnswer {
    Ok,
    /// 403 on a withdraw: not the registrant.
    NotRegistrant(String),
    /// 409 `already_consumed`.
    Consumed(String),
    Refused(String),
}

/// Coord's I/O, behind a seam so the decision logic is testable without a
/// network. `Err` means the call was not answered (transport, 5xx, parse).
#[async_trait::async_trait]
pub trait NetsCoord: Send + Sync {
    async fn list_plan_reserves(&self) -> Result<ReservePage, String>;
    async fn list_gates(&self, stem: &str) -> Result<Vec<GateRow>, String>;
    async fn read_gate(&self, gate_id: &str) -> Result<Option<GateRow>, String>;
    async fn write(&self, verb: GateWrite, gate_id: &str, reason: &str)
        -> Result<WriteAnswer, String>;
}

/// Whether this device is proven bound to a single tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TenantProof {
    Single,
    Unproven(String),
}

/// What the run needs to know about this device.
#[derive(Debug, Clone)]
pub struct RetireContext {
    /// This device's id (`machine.json`), lowercased; `None` when unreadable.
    pub device_id: Option<String>,
    pub tenant: TenantProof,
}

/// The run's deadline.
struct Budget {
    deadline: Instant,
    total: Duration,
}

impl Budget {
    /// Run `fut` cut at `min(cap, what is left)`; refused when less than
    /// [`CALL_FLOOR`] is left.
    async fn call<T>(
        &self,
        what: &str,
        fut: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left < CALL_FLOOR {
            return Err(format!(
                "the retire budget ({}s) is spent before {what}",
                self.total.as_secs()
            ));
        }
        let lim = CALL_CAP.min(left);
        match tokio::time::timeout(lim, fut).await {
            Ok(r) => r,
            Err(_) => Err(format!("{what} timed out after {}s", lim.as_secs())),
        }
    }
}

/// A `plan:` key's stem, when it fits the release script's stem grammar.
fn plan_stem(resource_key: &str) -> Option<&str> {
    let stem = resource_key.strip_prefix("plan:")?;
    let mut chars = stem.chars();
    let first = chars.next()?;
    let ok = first.is_ascii_alphanumeric()
        && stem.len() <= 200
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then_some(stem)
}

/// The decision logic, over an injected [`NetsCoord`].
pub async fn retire_nets_with(
    io: &dyn NetsCoord,
    ctx: &RetireContext,
    claude_session_id: &str,
    by: &str,
    budget: Duration,
) -> NetsOutcome {
    let budget = Budget {
        deadline: Instant::now() + budget,
        total: budget,
    };
    let target = claude_session_id.trim().to_ascii_lowercase();

    // 1. The target's reserves.
    let page = match budget
        .call("GET /coord/claims/list", io.list_plan_reserves())
        .await
    {
        Ok(p) => p,
        Err(e) => return NetsOutcome::unknown(format!("the claims list was unreadable: {e}")),
    };
    let mut stems: Vec<String> = page
        .holders
        .iter()
        .filter(|h| {
            h.session_id
                .as_deref()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case(&target))
        })
        .filter(|h| h.resource_key.starts_with("plan:"))
        .map(|h| h.resource_key.clone())
        .collect();
    stems.sort();
    stems.dedup();
    // A reserve the target holds whose key is not a stem this run can name
    // is still a reserve whose net may fire: UNKNOWN, never dropped to NONE.
    let (stems, odd): (Vec<String>, Vec<String>) = stems.into_iter().partition(|k| plan_stem(k).is_some());
    let stems: Vec<String> = stems
        .into_iter()
        .filter_map(|k| plan_stem(&k).map(str::to_string))
        .collect();

    let mut out = NetsOutcome {
        stems: stems.clone(),
        ..Default::default()
    };
    if page.truncated {
        out.unknown = Some(format!(
            "the claims list was truncated: {} plan reserve(s) handled, others may remain",
            stems.len()
        ));
    }
    for key in &odd {
        out.unreadable(
            key,
            None,
            "the target holds this reserve, but its key is not a plan stem this run can read gates for",
        );
    }
    if stems.is_empty() {
        return out;
    }

    // 2. The tenant rule, and the device the nets must be registered by.
    if let TenantProof::Unproven(why) = &ctx.tenant {
        for stem in &stems {
            out.unretire(stem, None, format!("owner tenant unproven: {why}"));
        }
        return out;
    }
    let Some(device) = ctx.device_id.as_deref().map(str::to_ascii_lowercase) else {
        for stem in &stems {
            out.unretire(
                stem,
                None,
                "this device's id is unreadable (machine.json), so no net can be proven registered here",
            );
        }
        return out;
    };

    let reason = format!(
        "implement\u{2192}ship net retired: finished by {by} (runner finish door)"
    );

    // 3-4. Each plan's nets.
    for stem in &stems {
        let rows = match budget
            .call("GET /coord/agent-gates", io.list_gates(stem))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                out.unreadable(stem, None, format!("the plan's gate read failed: {e}"));
                continue;
            }
        };
        for row in rows.into_iter().filter(GateRow::is_net) {
            if row.registered_by != device {
                out.inert(
                    stem,
                    &row.gate_id,
                    format!(
                        "NET_INERT: registered by another device ({}), left alone",
                        if row.registered_by.is_empty() { "none recorded" } else { row.registered_by.as_str() }
                    ),
                );
                continue;
            }
            retire_one(io, &budget, &mut out, stem, row, &reason).await;
        }
    }
    out
}

/// Act on one selected net (the script's `retire_one` + `settle`).
async fn retire_one(
    io: &dyn NetsCoord,
    budget: &Budget,
    out: &mut NetsOutcome,
    stem: &str,
    row: GateRow,
    reason: &str,
) {
    let id = row.gate_id.clone();
    match row.verdict.as_str() {
        "withdrawn" => return out.inert(stem, &id, "verdict=withdrawn: already retired"),
        "failed" | "misconfigured" => {
            return out.inert(
                stem,
                &id,
                format!("verdict={}: a non-clear terminal verdict dispatches nothing", row.verdict),
            )
        }
        "cleared" => return settle_cleared(io, budget, out, stem, row, reason).await,
        "open" => {}
        other => return out.unretire(stem, Some(&id), format!("the row reads an unrecognised verdict '{other}'")),
    }

    let withdraw = budget
        .call("the withdraw", io.write(GateWrite::Withdraw, &id, reason))
        .await;
    let mut not_registrant = false;
    let mut cancel_text = String::new();
    let withdraw_text = match &withdraw {
        Ok(WriteAnswer::Ok) => "withdrawn".to_string(),
        Ok(WriteAnswer::NotRegistrant(t)) => {
            not_registrant = true;
            // Never mute: a muted gate is skipped by the sweep and stays open
            // for ever (served coordination `gate-lifecycle-end`).
            cancel_text = match budget
                .call("the continuation cancel",
                    io.write(GateWrite::CancelContinuation, &id, reason),
                )
                .await
            {
                Ok(WriteAnswer::Ok) => "cancelled".to_string(),
                Ok(WriteAnswer::NotRegistrant(t) | WriteAnswer::Consumed(t) | WriteAnswer::Refused(t)) => t,
                Err(e) => e,
            };
            t.clone()
        }
        Ok(WriteAnswer::Consumed(t) | WriteAnswer::Refused(t)) => t.clone(),
        Err(e) => e.clone(),
    };

    let back = match budget
        .call("the read-back", io.read_gate(&id))
        .await
    {
        Ok(Some(b)) => b,
        Ok(None) => {
            return out.unretire(stem, Some(&id), "the read-back after the withdraw found no row")
        }
        Err(e) => {
            return out.unretire(
                stem,
                Some(&id),
                format!("the read-back after the withdraw could not be made ({e})"),
            )
        }
    };
    match back.verdict.as_str() {
        "withdrawn" => {
            if matches!(withdraw, Ok(WriteAnswer::Refused(_))) {
                out.inert(stem, &id, "verdict=withdrawn: already retired");
            } else {
                out.withdrawn.push(id);
            }
        }
        "failed" | "misconfigured" => out.inert(
            stem,
            &id,
            format!("verdict={}: a non-clear terminal verdict dispatches nothing", back.verdict),
        ),
        "cleared" => settle_cleared(io, budget, out, stem, back, reason).await,
        "open" if !not_registrant => out.unretire(
            stem,
            Some(&id),
            format!("the withdraw did not land ({withdraw_text}); the row still reads open"),
        ),
        "open" if back.continuation_live() => out.unretire(
            stem,
            Some(&id),
            format!(
                "the withdraw was refused as not-the-registrant ({withdraw_text}), and its continuation is still live after the cancel ({cancel_text})"
            ),
        ),
        "open" => out.cancelled.push(id),
        other => out.unretire(stem, Some(&id), format!("the row reads an unrecognised verdict '{other}'")),
    }
}

/// A net that already cleared: cancel a continuation that can still dispatch.
async fn settle_cleared(
    io: &dyn NetsCoord,
    budget: &Budget,
    out: &mut NetsOutcome,
    stem: &str,
    row: GateRow,
    reason: &str,
) {
    let id = row.gate_id.clone();
    if row.consumed {
        return out.inert(
            stem,
            &id,
            "cleared and its continuation consumed: read as a predecessor's net",
        );
    }
    if !row.continuation_live() {
        return out.inert(stem, &id, "verdict=cleared, continuation not live: nothing left to dispatch");
    }
    let why = match budget
        .call("the continuation cancel",
            io.write(GateWrite::CancelContinuation, &id, reason),
        )
        .await
    {
        Ok(WriteAnswer::Ok) => "cancelled".to_string(),
        Ok(WriteAnswer::NotRegistrant(t) | WriteAnswer::Consumed(t) | WriteAnswer::Refused(t)) => t,
        Err(e) => {
            return out.unretire(
                stem,
                Some(&id),
                format!("the net already cleared and its continuation is still live; the cancel was not carried ({e})"),
            )
        }
    };
    match budget
        .call("the read-back", io.read_gate(&id))
        .await
    {
        Ok(Some(back)) if back.consumed => out.inert(
            stem,
            &id,
            "cleared and its continuation consumed: read as a predecessor's net",
        ),
        Ok(Some(back)) if back.continuation_live() => out.unretire(
            stem,
            Some(&id),
            format!("the net already cleared and its continuation is still live after the cancel ({why})"),
        ),
        Ok(Some(_)) => out.cancelled.push(id),
        Ok(None) => out.unretire(stem, Some(&id), "the cancel's read-back found no row"),
        Err(e) => out.unretire(
            stem,
            Some(&id),
            format!("the net already cleared; the cancel's read-back could not be made ({e})"),
        ),
    }
}

// ===========================================================================
// Production
// ===========================================================================

/// Coord over HTTP: the anonymous claims list, and the device-JWT gate doors
/// (the credential and base [`coord_client_parts`] resolves).
///
/// [`coord_client_parts`]: crate::mcp::continuation_verdict::coord_client_parts
struct HttpNetsCoord {
    base: String,
    jwt: Option<String>,
    client: reqwest::Client,
}

fn body_head(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect()
}

impl HttpNetsCoord {
    fn bearer(&self) -> Result<&str, String> {
        self.jwt
            .as_deref()
            .ok_or_else(|| "no device JWT (unpaired), so no gate door".to_string())
    }

    async fn gates_page(&self, query: &str) -> Result<(Vec<GateRow>, bool), String> {
        let url = format!("{}/coord/agent-gates?{query}", self.base);
        let resp = self
            .client
            .get(&url)
            .timeout(CALL_CAP)
            .bearer_auth(self.bearer()?)
            .send()
            .await
            .map_err(|e| format!("GET /coord/agent-gates: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 404 {
            return Err(format!(
                "GET /coord/agent-gates answered 404 ({}): no work unit by that slug in the device JWT's tenant",
                body_head(&text)
            ));
        }
        if status != 200 {
            return Err(format!("GET /coord/agent-gates answered HTTP {status}: {}", body_head(&text)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("GET /coord/agent-gates: unparseable body ({e})"))?;
        let gates = v
            .get("gates")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "GET /coord/agent-gates: the body carries no `gates` array".to_string())?;
        let truncated = v
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "GET /coord/agent-gates: the body carries no `truncated` flag".to_string())?;
        Ok((gates.iter().filter_map(GateRow::from_json).collect(), truncated))
    }
}

#[async_trait::async_trait]
impl NetsCoord for HttpNetsCoord {
    async fn list_plan_reserves(&self) -> Result<ReservePage, String> {
        let url = format!(
            "{}/coord/claims/list?kind=semantic_resource&prefix=plan%3A&limit=500",
            self.base
        );
        let resp = self
            .client
            .get(&url)
            .timeout(CALL_CAP)
            .send()
            .await
            .map_err(|e| format!("GET /coord/claims/list: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(format!("GET /coord/claims/list answered HTTP {status}: {}", body_head(&text)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("GET /coord/claims/list: unparseable body ({e})"))?;
        let holders = v
            .get("holders")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "GET /coord/claims/list: the body carries no `holders` array".to_string())?;
        Ok(ReservePage {
            holders: holders
                .iter()
                .filter_map(|h| {
                    Some(ReserveRow {
                        resource_key: h.get("resource_key")?.as_str()?.to_string(),
                        session_id: h
                            .get("session_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect(),
            truncated: v.get("truncated").and_then(serde_json::Value::as_bool) == Some(true),
        })
    }

    async fn list_gates(&self, stem: &str) -> Result<Vec<GateRow>, String> {
        let mut rows = Vec::new();
        for page in 0..MAX_GATE_PAGES {
            let (mut got, truncated) = self
                .gates_page(&format!(
                    "work_unit_slug={}&limit=100&offset={}",
                    urlencoding::encode(stem),
                    page * 100
                ))
                .await?;
            rows.append(&mut got);
            if !truncated {
                return Ok(rows);
            }
        }
        Err(format!(
            "the plan has more than {} live gates, so the read stopped short of the set",
            MAX_GATE_PAGES * 100
        ))
    }

    async fn read_gate(&self, gate_id: &str) -> Result<Option<GateRow>, String> {
        let (rows, _) = self
            .gates_page(&format!(
                "gate_id={}&include_archived=true",
                urlencoding::encode(gate_id)
            ))
            .await?;
        Ok(rows.into_iter().find(|r| r.gate_id == gate_id))
    }

    async fn write(&self, verb: GateWrite, gate_id: &str, reason: &str) -> Result<WriteAnswer, String> {
        let path = match verb {
            GateWrite::Withdraw => format!("/coord/gates/{gate_id}/withdraw"),
            GateWrite::CancelContinuation => format!("/coord/gates/{gate_id}/agent/continuation-cancel"),
        };
        let resp = self
            .client
            .post(format!("{}{path}", self.base))
            .timeout(CALL_CAP)
            .bearer_auth(self.bearer()?)
            .json(&serde_json::json!({ "reason": reason }))
            .send()
            .await
            .map_err(|e| format!("POST {path}: {e}"))?;
        let status = resp.status().as_u16();
        let text = format!("HTTP {status} from POST {path}: {}", body_head(&resp.text().await.unwrap_or_default()));
        Ok(match status {
            200..=299 => WriteAnswer::Ok,
            403 if verb == GateWrite::Withdraw => WriteAnswer::NotRegistrant(text),
            409 if text.contains("already_consumed") => WriteAnswer::Consumed(text),
            500..=599 => return Err(text),
            _ => WriteAnswer::Refused(text),
        })
    }
}

/// This device's tenant proof and id. Blocking (small file reads and the
/// credential read), so it runs on the blocking pool.
fn read_context() -> (RetireContext, Option<String>, String) {
    use qontinui_runner_lib::plan_workunit_adapter::trigger::{
        read_device_binding_count, work_unit_write_posture, WorkUnitWritePosture,
    };
    let tenant = match work_unit_write_posture(read_device_binding_count()) {
        WorkUnitWritePosture::Write => TenantProof::Single,
        WorkUnitWritePosture::WithheldMultiBound(n) => TenantProof::Unproven(format!(
            "this device is bound to {n} tenants and the plan's owning tenant is not resolvable here"
        )),
        WorkUnitWritePosture::WithheldBindingsUnknown => TenantProof::Unproven(
            "coord's binding set for this device is UNKNOWN (no fresh record), so a single binding is not proven"
                .to_string(),
        ),
    };
    let device_id = qontinui_runner_lib::machine_identity::read_device_id()
        .ok()
        .map(|d| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty());
    let (base, jwt) = match crate::mcp::continuation_verdict::coord_client_parts() {
        Ok((base, jwt)) => (base, Some(jwt)),
        Err(_) => {
            let (base, _) = qontinui_runner_lib::profiles::coord_base_with_source();
            (base.trim_end_matches('/').to_string(), None)
        }
    };
    (RetireContext { device_id, tenant }, jwt, base)
}

/// Retire the implement→ship nets of every plan reserve `claude_session_id`
/// holds, before a runner finish door writes its mark. Bounded by
/// `budget` (the doors pass [`DOOR_BUDGET`]), the device-context read
/// included; never fails — every failure, the budget running out among them,
/// is in the outcome (UNKNOWN), and the caller writes the mark regardless.
pub async fn retire_nets_for_finish(claude_session_id: &str, by: &str, budget: Duration) -> NetsOutcome {
    let started = Instant::now();
    let (ctx, jwt, base) = match tokio::time::timeout(budget, tokio::task::spawn_blocking(read_context)).await {
        Ok(Ok(parts)) => parts,
        Ok(Err(e)) => return NetsOutcome::unknown(format!("the device context read panicked: {e}")),
        Err(_) => {
            return NetsOutcome::unknown(format!(
                "the device context read outlived the retire budget ({}s)",
                budget.as_secs()
            ))
        }
    };
    // No client-level timeout: every request carries its own [`CALL_CAP`].
    let client = match reqwest::Client::builder().build() {
        Ok(c) => c,
        Err(e) => return NetsOutcome::unknown(format!("no HTTP client: {e}")),
    };
    let io = HttpNetsCoord { base, jwt, client };
    let left = budget.saturating_sub(started.elapsed());
    let out = retire_nets_with(&io, &ctx, claude_session_id, by, left).await;
    let verdict = out.verdict();
    if matches!(verdict, NetsVerdict::NetUnretired | NetsVerdict::Unknown) {
        warn!(
            claude_session_id,
            by,
            verdict = verdict.as_str(),
            nets = %out.to_json(),
            "finish_net_retire: an implement→ship net was NOT retired before the finish mark"
        );
    } else {
        info!(
            claude_session_id,
            by,
            verdict = verdict.as_str(),
            withdrawn = ?out.withdrawn,
            cancelled = ?out.cancelled,
            "finish_net_retire: nets retired before the finish mark"
        );
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    const ME: &str = "4b426e2b-cc10-4870-80a5-524aaac70719";
    const OTHER: &str = "11111111-2222-3333-4444-555555555555";
    const TARGET: &str = "AAAA0000-1111-2222-3333-444444444444";

    fn net(id: &str, verdict: &str, by: &str) -> GateRow {
        GateRow {
            gate_id: id.to_string(),
            verdict: verdict.to_string(),
            predicate_kind: "claim_terminal".to_string(),
            phase_name: NET_PHASE_PREFIX.to_string(),
            registered_by: by.to_string(),
            will_dispatch: Some(true),
            consumed: false,
            cancelled: false,
            armed: Some(true),
        }
    }

    /// A scripted coord. Gate state moves on writes so read-backs see them.
    #[derive(Default)]
    pub(crate) struct FakeCoord {
        pub reserves: Option<ReservePage>,
        pub gates: Mutex<Vec<GateRow>>,
        pub gate_read_fails: bool,
        pub withdraw_answer: Option<WriteAnswer>,
        pub writes: Mutex<Vec<(GateWrite, String)>>,
    }

    #[async_trait::async_trait]
    impl NetsCoord for FakeCoord {
        async fn list_plan_reserves(&self) -> Result<ReservePage, String> {
            self.reserves.clone().ok_or_else(|| "unreachable".to_string())
        }
        async fn list_gates(&self, _stem: &str) -> Result<Vec<GateRow>, String> {
            if self.gate_read_fails {
                return Err("HTTP 503".to_string());
            }
            Ok(self.gates.lock().unwrap().clone())
        }
        async fn read_gate(&self, gate_id: &str) -> Result<Option<GateRow>, String> {
            Ok(self.gates.lock().unwrap().iter().find(|g| g.gate_id == gate_id).cloned())
        }
        async fn write(&self, verb: GateWrite, gate_id: &str, _reason: &str) -> Result<WriteAnswer, String> {
            self.writes.lock().unwrap().push((verb, gate_id.to_string()));
            let answer = match verb {
                GateWrite::Withdraw => self.withdraw_answer.clone().unwrap_or(WriteAnswer::Ok),
                GateWrite::CancelContinuation => WriteAnswer::Ok,
            };
            if answer == WriteAnswer::Ok {
                let mut g = self.gates.lock().unwrap();
                if let Some(row) = g.iter_mut().find(|r| r.gate_id == gate_id) {
                    match verb {
                        GateWrite::Withdraw => row.verdict = "withdrawn".to_string(),
                        GateWrite::CancelContinuation => {
                            row.cancelled = true;
                            row.will_dispatch = Some(false);
                        }
                    }
                }
            }
            Ok(answer)
        }
    }

    pub(crate) fn reserve(stem: &str, session: &str) -> ReserveRow {
        ReserveRow {
            resource_key: format!("plan:{stem}"),
            session_id: Some(session.to_string()),
        }
    }

    fn single() -> RetireContext {
        RetireContext {
            device_id: Some(ME.to_string()),
            tenant: TenantProof::Single,
        }
    }

    async fn run(io: &FakeCoord, ctx: &RetireContext) -> NetsOutcome {
        retire_nets_with(io, ctx, TARGET, "test", DOOR_BUDGET).await
    }

    fn page(holders: Vec<ReserveRow>) -> Option<ReservePage> {
        Some(ReservePage { holders, truncated: false })
    }

    #[tokio::test]
    async fn one_open_net_is_withdrawn_and_read_back() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", &TARGET.to_lowercase()), reserve("p2", OTHER)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(out.stems, vec!["p1".to_string()], "only the target's reserve, matched case-insensitively");
        assert_eq!(out.withdrawn, vec!["g1".to_string()]);
        assert_eq!(out.verdict(), NetsVerdict::Retired);
        assert_eq!(*io.writes.lock().unwrap(), vec![(GateWrite::Withdraw, "g1".to_string())]);
        assert_eq!(out.to_json()["verdict"], "RETIRED");
    }

    #[tokio::test]
    async fn no_reserve_is_none_and_touches_nothing() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p2", OTHER)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(out.verdict(), NetsVerdict::None);
        assert!(io.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_truncated_list_with_no_match_is_unknown_not_none() {
        let io = FakeCoord {
            reserves: Some(ReservePage { holders: vec![], truncated: true }),
            ..Default::default()
        };
        assert_eq!(run(&io, &single()).await.verdict(), NetsVerdict::Unknown);
    }

    #[tokio::test]
    async fn an_unreadable_claims_list_is_unknown() {
        let io = FakeCoord::default();
        assert_eq!(run(&io, &single()).await.verdict(), NetsVerdict::Unknown);
    }

    #[tokio::test]
    async fn an_unreadable_gate_read_is_unknown() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gate_read_fails: true,
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(out.verdict(), NetsVerdict::Unknown);
        assert_eq!(out.unretired.len(), 1);
        assert_eq!(out.unretired[0].gate_id, None);
    }

    #[tokio::test]
    async fn another_devices_net_is_inert_and_untouched() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "open", OTHER)]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert!(io.writes.lock().unwrap().is_empty());
        assert_eq!(out.inert.len(), 1);
        assert!(out.inert[0].why.starts_with("NET_INERT"));
        assert_eq!(out.verdict(), NetsVerdict::Retired);
    }

    #[tokio::test]
    async fn a_non_net_gate_is_ignored() {
        let mut other = net("g2", "open", ME);
        other.phase_name = "vet\u{2192}implement safety net".to_string();
        let mut timed = net("g3", "open", ME);
        timed.predicate_kind = "time_elapsed".to_string();
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![other, timed]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert!(io.writes.lock().unwrap().is_empty());
        assert!(out.inert.is_empty() && out.withdrawn.is_empty());
    }

    #[tokio::test]
    async fn an_unproven_tenant_withdraws_nothing_and_reads_unretired() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            ..Default::default()
        };
        let ctx = RetireContext {
            device_id: Some(ME.to_string()),
            tenant: TenantProof::Unproven("bound to 2 tenants".to_string()),
        };
        let out = run(&io, &ctx).await;
        assert!(io.writes.lock().unwrap().is_empty());
        assert_eq!(out.verdict(), NetsVerdict::NetUnretired);
        assert!(out.unretired[0].why.starts_with("owner tenant unproven"));
    }

    #[tokio::test]
    async fn a_not_registrant_refusal_cancels_and_never_mutes() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            withdraw_answer: Some(WriteAnswer::NotRegistrant("HTTP 403".to_string())),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(
            *io.writes.lock().unwrap(),
            vec![
                (GateWrite::Withdraw, "g1".to_string()),
                (GateWrite::CancelContinuation, "g1".to_string())
            ]
        );
        assert_eq!(out.cancelled, vec!["g1".to_string()]);
        assert_eq!(out.verdict(), NetsVerdict::Retired);
    }

    #[tokio::test]
    async fn a_cleared_unconsumed_net_gets_its_continuation_cancelled() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "cleared", ME)]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(*io.writes.lock().unwrap(), vec![(GateWrite::CancelContinuation, "g1".to_string())]);
        assert_eq!(out.cancelled, vec!["g1".to_string()]);
    }

    #[tokio::test]
    async fn a_withdraw_that_does_not_land_is_net_unretired() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            withdraw_answer: Some(WriteAnswer::Refused("HTTP 400".to_string())),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(out.verdict(), NetsVerdict::NetUnretired);
        assert_eq!(out.unretired[0].gate_id.as_deref(), Some("g1"));
    }

    #[test]
    fn gate_rows_parse_from_the_agent_gates_shape() {
        let row = GateRow::from_json(&serde_json::json!({
            "gate_id": "96E61CDE-4092-455B-83B8-94EA4F1B6B95",
            "verdict": "open",
            "phase_name": "implement\u{2192}ship safety net",
            "registered_by": ME,
            "predicate": {"kind": "claim_terminal", "resource_key": "plan:x"},
            "continuation_will_dispatch": true,
            "continuation_consumed_at": null,
            "continuation_cancelled_at": null,
            "continuation_spawn": {"kind": "run_skill"},
        }))
        .unwrap();
        assert!(row.is_net());
        assert!(row.continuation_live());
        assert_eq!(row.gate_id, "96e61cde-4092-455b-83b8-94ea4f1b6b95");
    }

    #[tokio::test]
    async fn a_held_reserve_whose_key_is_not_a_stem_is_unknown_not_none() {
        let io = FakeCoord {
            reserves: page(vec![ReserveRow {
                resource_key: "plan:a/b".to_string(),
                session_id: Some(TARGET.to_string()),
            }]),
            ..Default::default()
        };
        let out = run(&io, &single()).await;
        assert_eq!(out.verdict(), NetsVerdict::Unknown);
        assert_eq!(out.unretired[0].stem, "plan:a/b");
        assert!(io.writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_spent_budget_is_unknown_and_writes_nothing() {
        let io = FakeCoord {
            reserves: page(vec![reserve("p1", TARGET)]),
            gates: Mutex::new(vec![net("g1", "open", ME)]),
            ..Default::default()
        };
        // Below the per-call floor: no call starts at all.
        let out = retire_nets_with(&io, &single(), TARGET, "test", Duration::from_secs(1)).await;
        assert_eq!(out.verdict(), NetsVerdict::Unknown);
        assert!(io.writes.lock().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // HttpNetsCoord against a stub coord on 127.0.0.1:0
    // -----------------------------------------------------------------------

    /// What the stub coord holds and saw.
    #[derive(Default)]
    struct StubCoord {
        /// Gate rows in `GET /coord/agent-gates`' flat shape.
        gates: Mutex<Vec<serde_json::Value>>,
        /// Every `METHOD path?query` it answered.
        seen: Mutex<Vec<String>>,
        claims_truncated: bool,
        /// Serve the plan's gates as two pages (offset 0 truncated, 100 not).
        two_pages: bool,
    }

    fn stub_gate(id: &str, verdict: &str) -> serde_json::Value {
        serde_json::json!({
            "gate_id": id,
            "verdict": verdict,
            "phase_name": NET_PHASE_PREFIX,
            "registered_by": ME,
            "predicate": {"kind": "claim_terminal"},
            "continuation_will_dispatch": true,
            "continuation_consumed_at": null,
            "continuation_cancelled_at": null,
            "continuation_spawn": {"kind": "run_skill"},
        })
    }

    async fn stub_handler(
        axum::extract::State(st): axum::extract::State<Arc<StubCoord>>,
        method: axum::http::Method,
        uri: axum::http::Uri,
    ) -> axum::response::Response {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        let path = uri.path().to_string();
        let query = uri.query().unwrap_or("").to_string();
        st.seen.lock().unwrap().push(format!("{method} {path}?{query}"));
        let json = |code: StatusCode, v: serde_json::Value| (code, axum::Json(v)).into_response();
        let param = |k: &str| {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
        };
        if path == "/coord/claims/list" {
            return json(
                StatusCode::OK,
                serde_json::json!({
                    "holders": [{"resource_key": "plan:p1", "session_id": TARGET.to_lowercase()}],
                    "truncated": st.claims_truncated,
                }),
            );
        }
        if path == "/coord/agent-gates" {
            let gates = st.gates.lock().unwrap().clone();
            if let Some(id) = param("gate_id") {
                let rows: Vec<_> = gates.into_iter().filter(|g| g["gate_id"] == id.as_str()).collect();
                return json(StatusCode::OK, serde_json::json!({"gates": rows, "truncated": false}));
            }
            if st.two_pages {
                let first = param("offset").as_deref() == Some("0");
                let row = if first { gates[0].clone() } else { gates[1].clone() };
                return json(StatusCode::OK, serde_json::json!({"gates": [row], "truncated": first}));
            }
            return json(StatusCode::OK, serde_json::json!({"gates": gates, "truncated": false}));
        }
        if let Some(rest) = path.strip_prefix("/coord/gates/") {
            let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
            match (id, action) {
                ("g403", "withdraw") => return json(StatusCode::FORBIDDEN, serde_json::json!({"error": "not_registrant"})),
                ("g409", "withdraw") => return json(StatusCode::CONFLICT, serde_json::json!({"error": "already_consumed"})),
                ("g500", "withdraw") => return json(StatusCode::BAD_GATEWAY, serde_json::json!({"error": "upstream"})),
                (_, "withdraw") | (_, "agent/continuation-cancel") => {
                    let mut gates = st.gates.lock().unwrap();
                    if let Some(g) = gates.iter_mut().find(|g| g["gate_id"] == id) {
                        if action == "withdraw" {
                            g["verdict"] = "withdrawn".into();
                        } else {
                            g["continuation_will_dispatch"] = false.into();
                            g["continuation_cancelled_at"] = "2026-11-23T00:00:00Z".into();
                        }
                    }
                    return json(StatusCode::OK, serde_json::json!({"ok": true}));
                }
                _ => {}
            }
        }
        json(StatusCode::NOT_FOUND, serde_json::json!({"error": "no route"}))
    }

    async fn serve_stub(st: Arc<StubCoord>) -> HttpNetsCoord {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(stub_handler).with_state(st);
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        HttpNetsCoord {
            base: format!("http://{addr}"),
            jwt: Some("test-jwt".to_string()),
            client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn http_claims_list_carries_the_truncated_flag() {
        let st = Arc::new(StubCoord {
            claims_truncated: true,
            ..Default::default()
        });
        let io = serve_stub(st.clone()).await;
        let page = io.list_plan_reserves().await.unwrap();
        assert!(page.truncated);
        assert_eq!(page.holders, vec![reserve("p1", &TARGET.to_lowercase())]);
        assert!(st.seen.lock().unwrap()[0]
            .contains("kind=semantic_resource&prefix=plan%3A&limit=500"));
    }

    #[tokio::test]
    async fn http_gate_read_walks_pages_until_not_truncated() {
        let st = Arc::new(StubCoord {
            two_pages: true,
            gates: Mutex::new(vec![stub_gate("g1", "open"), stub_gate("g2", "cleared")]),
            ..Default::default()
        });
        let io = serve_stub(st.clone()).await;
        let rows = io.list_gates("p1").await.unwrap();
        assert_eq!(rows.iter().map(|r| r.gate_id.as_str()).collect::<Vec<_>>(), ["g1", "g2"]);
        let seen = st.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].contains("work_unit_slug=p1&limit=100&offset=0"));
        assert!(seen[1].contains("offset=100"));
        assert!(seen.iter().all(|q| !q.contains("open_only")), "the route refuses unknown keys");
    }

    #[tokio::test]
    async fn http_withdraw_403_falls_back_to_cancel_and_never_mutes() {
        let st = Arc::new(StubCoord {
            gates: Mutex::new(vec![stub_gate("g403", "open")]),
            ..Default::default()
        });
        let io = serve_stub(st.clone()).await;
        assert!(matches!(
            io.write(GateWrite::Withdraw, "g403", "r").await,
            Ok(WriteAnswer::NotRegistrant(_))
        ));
        let out = retire_nets_with(&io, &single(), TARGET, "test", DOOR_BUDGET).await;
        assert_eq!(out.cancelled, vec!["g403".to_string()], "{:?}", out);
        assert_eq!(out.verdict(), NetsVerdict::Retired);
        let seen = st.seen.lock().unwrap().clone();
        assert!(seen.iter().any(|q| q.starts_with("POST /coord/gates/g403/agent/continuation-cancel")));
        assert!(seen.iter().all(|q| !q.contains("mute")));
    }

    #[tokio::test]
    async fn http_write_answers_map_409_and_5xx() {
        let io = serve_stub(Arc::new(StubCoord::default())).await;
        assert!(matches!(
            io.write(GateWrite::Withdraw, "g409", "r").await,
            Ok(WriteAnswer::Consumed(_))
        ));
        assert!(io.write(GateWrite::Withdraw, "g500", "r").await.is_err(), "a 5xx is unanswered");
        assert_eq!(
            io.write(GateWrite::Withdraw, "g1", "r").await,
            Ok(WriteAnswer::Ok)
        );
    }

    #[test]
    fn plan_stems_follow_the_release_scripts_grammar() {
        assert_eq!(plan_stem("plan:2026-11-23-a.b_c"), Some("2026-11-23-a.b_c"));
        assert_eq!(plan_stem("plan:-x"), None);
        assert_eq!(plan_stem("plan:a/b"), None);
        assert_eq!(plan_stem("phase:x"), None);
    }

    // -----------------------------------------------------------------------
    // The door contract — every finish door runs it (one test per door).
    // -----------------------------------------------------------------------

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// A retirer that records WHEN it was called (the target's `finished_at`
    /// at that moment) and runs the real decision logic over a [`FakeCoord`].
    pub(crate) struct OrderProbe {
        store: Arc<SessionLifecycleStore>,
        coord: FakeCoord,
        calls: Mutex<Vec<(String, Option<i64>)>>,
    }

    #[async_trait::async_trait]
    impl FinishNetRetirer for OrderProbe {
        async fn retire(&self, claude_session_id: &str, by: &str, budget: Duration) -> NetsOutcome {
            let at_call = self.store.get(claude_session_id).and_then(|r| r.finished_at);
            self.calls
                .lock()
                .unwrap()
                .push((claude_session_id.to_string(), at_call));
            retire_nets_with(&self.coord, &single(), claude_session_id, by, budget).await
        }
    }

    /// A door under test: (store, retirer, id, finished) -> the response body,
    /// or `None` for a door that answers none (the relay).
    pub(crate) type DoorFn = fn(
        Arc<SessionLifecycleStore>,
        Arc<OrderProbe>,
        &'static str,
        bool,
    ) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>;

    const ID: &str = "door-session";

    fn fresh_store() -> (Arc<SessionLifecycleStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap());
        store.record_open(
            serde_json::from_value(serde_json::json!({
                "claudeSessionId": ID,
                "pageId": "default",
                "zoneIndex": 0,
                "terminalId": format!("term-{ID}"),
                "openedAt": 0,
                "lastSeenAt": 0,
                "state": "open",
            }))
            .unwrap(),
        );
        (store, dir)
    }

    fn probe(store: &Arc<SessionLifecycleStore>, coord: FakeCoord) -> Arc<OrderProbe> {
        Arc::new(OrderProbe {
            store: store.clone(),
            coord,
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Run one finish through `door` with `coord`; assert the retire ran ONCE
    /// and BEFORE the mark, and that the mark is set whatever it answered.
    async fn finish_through(door: DoorFn, coord: FakeCoord) -> (Arc<OrderProbe>, Option<serde_json::Value>) {
        let (store, _dir) = fresh_store();
        let p = probe(&store, coord);
        let body = door(store.clone(), p.clone(), ID, true).await;
        assert_eq!(
            *p.calls.lock().unwrap(),
            vec![(ID.to_string(), None)],
            "the door must retire nets exactly once, BEFORE set_finished writes the mark"
        );
        assert!(
            store.get(ID).unwrap().finished_at.is_some(),
            "a net outcome never blocks the mark"
        );
        (p, body)
    }

    /// The invariant "the withdraw completes, or is recorded UNRETIRED, before
    /// set_finished is called", over every case Phase 1 item 5 names.
    pub(crate) async fn door_contract(door: DoorFn, answers_a_body: bool, has_unmark: bool) {
        let nets = |body: &Option<serde_json::Value>| {
            assert_eq!(body.is_some(), answers_a_body);
            body.as_ref().map(|b| b["nets"].clone())
        };

        // One open net is withdrawn.
        let (p, body) = finish_through(
            door,
            FakeCoord {
                reserves: page(vec![reserve("p1", ID)]),
                gates: Mutex::new(vec![net("g1", "open", ME)]),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(*p.coord.writes.lock().unwrap(), vec![(GateWrite::Withdraw, "g1".to_string())]);
        if let Some(n) = nets(&body) {
            assert_eq!(n["verdict"], "RETIRED");
            assert_eq!(n["withdrawn"], serde_json::json!(["g1"]));
        }

        // No reserve withdraws nothing.
        let (p, body) = finish_through(
            door,
            FakeCoord {
                reserves: page(vec![reserve("p1", OTHER)]),
                gates: Mutex::new(vec![net("g1", "open", ME)]),
                ..Default::default()
            },
        )
        .await;
        assert!(p.coord.writes.lock().unwrap().is_empty());
        if let Some(n) = nets(&body) {
            assert_eq!(n["verdict"], "NONE");
        }

        // An unreadable gate read leaves the mark and reports UNKNOWN.
        let (p, body) = finish_through(
            door,
            FakeCoord {
                reserves: page(vec![reserve("p1", ID)]),
                gate_read_fails: true,
                ..Default::default()
            },
        )
        .await;
        assert!(p.coord.writes.lock().unwrap().is_empty());
        if let Some(n) = nets(&body) {
            assert_eq!(n["verdict"], "UNKNOWN");
            assert_eq!(n["unretired"][0]["stem"], "p1");
            assert!(n["unretired"][0]["gate_id"].is_null());
        }

        // Another device's net is NET_INERT and untouched.
        let (p, body) = finish_through(
            door,
            FakeCoord {
                reserves: page(vec![reserve("p1", ID)]),
                gates: Mutex::new(vec![net("g1", "open", OTHER)]),
                ..Default::default()
            },
        )
        .await;
        assert!(p.coord.writes.lock().unwrap().is_empty());
        if let Some(n) = nets(&body) {
            assert_eq!(n["inert"][0]["gate_id"], "g1");
        }

        // An unmark never calls the helper, and says the net is not re-armed.
        if has_unmark {
            let (store, _dir) = fresh_store();
            store.set_finished(ID, true, Some("done".to_string())).unwrap();
            let p = probe(
                &store,
                FakeCoord {
                    reserves: page(vec![reserve("p1", ID)]),
                    gates: Mutex::new(vec![net("g1", "open", ME)]),
                    ..Default::default()
                },
            );
            let body = door(store.clone(), p.clone(), ID, false).await;
            assert!(p.calls.lock().unwrap().is_empty(), "an unmark must never retire nets");
            assert!(store.get(ID).unwrap().finished_at.is_none());
            if let Some(n) = nets(&body) {
                assert_eq!(n["rearm"], REARM_NOTE);
            }
        }
    }

    #[test]
    fn an_unmark_reports_the_rearm_note() {
        assert_eq!(NetsReport::Unmark.to_json()["rearm"], REARM_NOTE);
    }
}
