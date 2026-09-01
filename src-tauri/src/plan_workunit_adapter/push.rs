//! Work-unit push client (Phase 2).
//!
//! Turns a parsed work-unit ([`super::parser::ParsedWorkUnit`]) into calls
//! against the P1 work-unit API:
//!
//! - `POST /coord/work-units/upsert`                  — slug-keyed upsert
//! - `POST /coord/work-units/:slug/transition`        — guarded status transition
//! - `GET  /coord/agent-work-units?slug_prefix=…`     — read current status
//! - `GET  /coord/agent-work-units/:slug/history`     — read the owning actor
//!
//! **The two READS are on the `agent-work-units` doors, not the bare
//! `work-units` ones.** coord moved its `GET /coord/work-units*` reads onto a
//! `TenantId` (operator-Cognito) sub-router so the dashboard could reach them;
//! an agent presenting a device JWT gets `403 tenant_not_resolved` there. The
//! device-authed twins (`get_list_agent` / `get_history_agent`) run the SAME
//! `list_response` / `history_response` cores over the same rows — the split is
//! about who may knock, not about what is served. This adapter was simply left
//! behind when the reads moved, which cost every `current_status` and
//! `last_actor` call in this module a 403 (plan
//! `2026-08-31-plan-adapter-retry-classification-unified`, Phase 0). The WRITES
//! stay on `/coord/work-units/*`: there is no agent read-door twin for a write,
//! and those routes were never moved.
//!
//! All authed with the runner's device-JWT via [`crate::auth::attach_device_auth`]
//! (the same bearer the rest of the runner's coord calls present) — the runner
//! step is server-side, so it holds the device JWT directly and does NOT need
//! the loopback write-forwarder (which serves the claim-anchored gate
//! `register`/`attest` and —
//! since the device-session coord-surface-hardening follow-up — the work-unit
//! registry routes, for nonce-only in-terminal sessions).
//!
//! ## Edge-triggered + idempotent
//!
//! The push carries a client-side **last-applied-status** memory (the
//! `last_applied` argument, threaded by [`super::trigger`]), mirroring coord's
//! old `ingested_status` edge-trigger but now on the client. A re-push of an
//! unchanged file yields NO transition (no phantom history row); only a status
//! that differs from what we last applied emits a `transition` call.
//!
//! ## Loud conflict surfacing
//!
//! Before applying a status edge we read the remote status. If it diverged from
//! what we last applied (someone — e.g. an agent — transitioned the unit
//! directly), we surface it LOUDLY (warn + a conflict counter in
//! [`super::trigger`]) and let the file win, exactly as coord's worker did.

use super::parser::ParsedWorkUnit;
use anyhow::{Context, Result};
use serde::Serialize;

/// Actor stamped on adapter-driven upserts/transitions.
pub const ADAPTER_ACTOR: &str = "harness-markdown-adapter";

/// Page size REQUESTED by the slug-prefix existence scan in
/// [`HttpWorkUnitSink::current_status`]. Only the request side: the truncation
/// guard compares against the limit coord ECHOES back, so lowering coord's own
/// ceiling cannot silently make the guard unreachable.
const PREFIX_SCAN_LIMIT: usize = 500;

/// Percent-encode a value for use inside a URL query string.
///
/// The slug comes from a FILENAME STEM ([`super::parser::slug_from_filename`]),
/// which sanitises nothing — a stem containing `#`, `&`, `%`, `+` or a space
/// would otherwise produce a well-formed 200 whose page cannot contain the
/// unit, and [`HttpWorkUnitSink::current_status`] would report that as a proven
/// absence. Since `Ok(None)` licenses the one write arm the agent-owner
/// deferral does not gate, the encoding is a correctness requirement, not
/// tidiness. Unreserved set per RFC 3986 §2.3.
fn percent_encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `current_status` read's URL: the DEVICE-authed work-unit list door,
/// filtered to one slug. Split out purely so the door it names is pinned by a
/// test — a silent drift back onto the operator-tier `/coord/work-units` route
/// costs a 403 on every call and, before this plan's Phase 0, did so silently.
fn current_status_url(base: &str, slug: &str) -> String {
    format!(
        "{base}/coord/agent-work-units?slug_prefix={}&limit={PREFIX_SCAN_LIMIT}",
        percent_encode_query_value(slug)
    )
}

/// The `last_actor` read's URL: the DEVICE-authed status-history door. Split
/// out for the same reason as [`current_status_url`].
fn last_actor_url(base: &str, slug: &str) -> String {
    format!("{base}/coord/agent-work-units/{slug}/history")
}

/// Pure reader for `GET /coord/agent-work-units`'s body: the unit's status,
/// `None` if the unit is provably ABSENT, and `Err` when the body cannot prove
/// either.
///
/// Split out of [`HttpWorkUnitSink::current_status`] so all four judgements —
/// an unrecognized envelope, an object with no rows array, a truncated page, and
/// a present row whose `status` is null — are testable without HTTP. They gate a
/// write that can overwrite a status an agent set, so "covered by a fake sink
/// that never runs this code" was not coverage.
///
/// `requested_limit` is the page size we ASKED for; the guard prefers the
/// `limit` coord echoes in the envelope, so a server that clamps lower is
/// detected rather than assumed away.
fn status_from_list_body(
    body: &serde_json::Value,
    slug: &str,
    requested_limit: usize,
) -> Result<Option<String>> {
    // Tolerant of array or {units|work_units: [...]} envelope — but ONLY of
    // those two. An envelope this reader does not recognize must be an ERROR
    // (UNKNOWN), never a silent zero rows read as "the unit does not exist".
    let rows: &Vec<serde_json::Value> = match body {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => o
            .get("units")
            .or_else(|| o.get("work_units"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "GET /coord/agent-work-units returned an object with no `units`/`work_units` \
                     array; refusing to read that as an absent unit"
                )
            })?,
        other => anyhow::bail!(
            "GET /coord/agent-work-units returned an unrecognized envelope ({}); refusing to \
             read it as an absent unit",
            match other {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::String(_) => "string",
                _ => "unknown",
            }
        ),
    };
    for row in rows {
        if row.get("slug").and_then(|s| s.as_str()) == Some(slug) {
            // The row EXISTS, so this is never `None`. A null/absent `status`
            // field is the empty-string seed coord writes on a fresh insert — an
            // unset status on a PRESENT unit, not an absent unit.
            return Ok(Some(
                row.get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
    }
    // A FULL page with no exact match means the prefix scan was truncated — the
    // unit may be on a page we never asked for. That is UNKNOWN, and reporting
    // it as absent would licence the unconditional status write. Compare
    // against the limit coord APPLIED (it echoes one) rather than the constant
    // we sent, so a server that clamps to a smaller page still trips the guard.
    let applied_limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .filter(|v| *v > 0)
        .unwrap_or(requested_limit);
    if rows.len() >= applied_limit {
        anyhow::bail!(
            "GET /coord/agent-work-units?slug_prefix={slug} returned a full page ({} rows, \
             limit {applied_limit}) with no exact match — the scan was truncated, so whether \
             this unit exists is UNKNOWN",
            rows.len()
        );
    }
    Ok(None)
}

/// A coord call this runner's principal is **not permitted** to make.
///
/// Distinct from a transport failure or a 5xx, both of which are worth
/// retrying: a `403` is coord's settled verdict about *this principal on this
/// route*, and re-sending the byte-identical request on the next reconcile
/// cycle cannot change it. Left untyped, the adapter re-issued (and re-logged)
/// every refused slug once per cycle forever — the dominant contributor to the
/// runner's log volume.
///
/// [`super::trigger::reconcile_once`] downcasts to this type to retire the slug
/// for the life of the process. It is deliberately process-scoped and not
/// persisted: the refusal can be a *transient* property of who last touched the
/// unit (coord's separation-of-duties check is the common source), so a fresh
/// process re-probes rather than inheriting a stale verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForbiddenByCoord {
    /// The route that refused, e.g. `POST /coord/work-units/upsert`.
    pub route: &'static str,
    /// coord's response body, verbatim — it carries the `error` discriminant
    /// (e.g. `self_attestation_forbidden`) that says *why*.
    pub detail: String,
}

impl std::fmt::Display for ForbiddenByCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> 403 {}", self.route, self.detail)
    }
}

impl std::error::Error for ForbiddenByCoord {}

/// Turn a non-success response into an error, typing a `403` as
/// [`ForbiddenByCoord`] so the caller can retire the slug instead of retrying.
///
/// Every non-2xx sink response goes through here so the classification is made
/// in exactly one place; a route that hand-rolled its own `bail!` would silently
/// re-open the retry storm this exists to close.
async fn classify_failure(route: &'static str, resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let detail = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::FORBIDDEN {
        return anyhow::Error::new(ForbiddenByCoord { route, detail });
    }
    anyhow::anyhow!("{route} -> {status} {detail}")
}

/// What one [`push_work_unit_with_remote`] call knows about the unit's status
/// **in coord** — THREE states, because there are three.
///
/// This replaces a `Option<Option<String>>` whose inner `None` meant "absent OR
/// the read failed". That collapse was written down as intended behaviour and
/// it was the defect: `current_status` returning `Err` was consumed as "no
/// remote status to compare against", which is indistinguishable from "the
/// remote agrees with us". Conflict detection was therefore dead across the
/// whole corpus and emitted no log line at all — a failed read looked exactly
/// like a clean cycle (plan
/// `2026-08-31-plan-adapter-retry-classification-unified`, Phase 0).
#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteStatus {
    /// Not read yet in this push. Read it if an arm needs it — at most once.
    Unread,
    /// Read, and the answer is trustworthy: `Some(status)` = the unit exists
    /// with that status, `None` = the unit is **provably** absent (the
    /// [`WorkUnitSink::current_status`] contract: anything that cannot prove
    /// absence returns `Err`, which lands in [`RemoteStatus::Unreadable`]).
    Known(Option<String>),
    /// The read FAILED. Whether the remote diverged is **UNKNOWN**. It is not
    /// "unchanged", it is not "absent", and it is not convergence — every arm
    /// below must either abstain loudly or take the conservative branch.
    Unreadable,
}

/// Read the unit's remote status, converting a failure into
/// [`RemoteStatus::Unreadable`] **with a loud warning** rather than into
/// silence.
///
/// Same shape as [`super::trigger::backfill_work_units_once`]'s seed read —
/// explicit `match`, `tracing::warn!`, abstain — deliberately, so the two sites
/// that must not swallow this error do not swallow it two different ways.
/// `site` names which arm asked, because the two arms respond to UNKNOWN
/// differently and a bare slug would not say which one is degraded.
async fn read_remote_status<S: WorkUnitSink + ?Sized>(
    sink: &S,
    slug: &str,
    site: &'static str,
) -> RemoteStatus {
    match sink.current_status(slug).await {
        Ok(s) => RemoteStatus::Known(s),
        Err(e) => {
            tracing::warn!(
                slug = %slug,
                site = %site,
                error = %format!("{e:#}"),
                "plan adapter: cannot read the remote work-unit status; this push's remote \
                 comparison is UNKNOWN, NOT 'unchanged' — a divergence, if there is one, is \
                 invisible this cycle"
            );
            RemoteStatus::Unreadable
        }
    }
}

/// True when `by_actor` denotes a real (non-proxy) actor that owns the unit —
/// i.e. anything other than this adapter's own actor (and not empty). Used to
/// decide whether the markdown proxy should DEFER its transition so it does not
/// collapse an agent-initiated transition back to the system actor.
fn is_real_agent_actor(by_actor: &str) -> bool {
    by_actor != ADAPTER_ACTOR && !by_actor.is_empty()
}

/// `POST /coord/work-units/upsert` body. Mirrors coord's `UpsertRequest`;
/// `None` fields are omitted so an upsert that only refreshes title/metadata
/// does not clobber `status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpsertBody {
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_actor: Option<String>,
}

/// `POST /coord/work-units/:slug/transition` body. Mirrors coord's
/// `TransitionRequest`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransitionBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_status: Option<String>,
    pub to_status: String,
    pub by_actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Build the `metadata` JSON pushed alongside the work-unit: the enrichment
/// coord's old slug+status projection discarded — phase sub-units, dependency
/// edges, and the source-file back-link.
pub fn build_metadata(u: &ParsedWorkUnit) -> serde_json::Value {
    serde_json::json!({
        "depends_on": u.depends_on,
        "phases": u
            .phases
            .iter()
            .map(|p| serde_json::json!({"index": p.index, "name": p.name}))
            .collect::<Vec<_>>(),
        "source_path": u.source_path,
    })
}

/// The pure edge-trigger decision: given the file's parsed status and the
/// status we last applied for this slug, what should the push do?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAction {
    /// No prior application by us — upsert WITH status (creates the row /
    /// sets the initial status).
    UpsertWithStatus,
    /// Status unchanged since our last application — refresh title/metadata
    /// only; no transition, no history row (idempotent re-sync).
    RefreshOnly,
    /// The file's status changed since our last application — transition.
    Transition { from: String, to: String },
}

/// Pure edge-trigger: mirrors coord's `decide_status_action`, client-side.
pub fn decide_push(parsed_status: &str, last_applied: Option<&str>) -> PushAction {
    match last_applied {
        None => PushAction::UpsertWithStatus,
        Some(prev) if prev == parsed_status => PushAction::RefreshOnly,
        Some(prev) => PushAction::Transition {
            from: prev.to_string(),
            to: parsed_status.to_string(),
        },
    }
}

/// What a single [`push_work_unit`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcomeKind {
    Created,
    Refreshed,
    Transitioned {
        from: String,
        to: String,
    },
    /// A `Transition` the graduation-bootstrap deferral suppressed: a real
    /// (non-adapter) agent last drove this unit, so the markdown proxy emitted
    /// nothing. Distinct from [`PushOutcomeKind::Refreshed`], which it used to
    /// masquerade as — a deferral is a WRITE THAT DID NOT HAPPEN and a refresh
    /// is a write that did, and collapsing the two made the single most
    /// interesting outcome of a reconcile cycle uncountable.
    Deferred {
        /// `by_actor` of the unit's newest status-history row.
        owner: String,
        /// The status the file wanted to apply, and did not.
        wanted: String,
    },
}

/// Result of pushing one work-unit, including whether a remote conflict was
/// detected (a direct write diverged from our last-applied status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    pub slug: String,
    pub kind: PushOutcomeKind,
    pub conflict: bool,
}

/// Outcome of a [`WorkUnitSink::set_deps`] call. Distinguishes the benign
/// "table not migrated yet" 503 (the edge table hasn't landed — the
/// `metadata.depends_on` JSONB fallback covers it, so this is NOT an error)
/// from a real applied write or a hard error (returned as `Err`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetDepsOutcome {
    /// Edges were set (2xx). `edges_set` is coord's reported count.
    Ok { edges_set: u64 },
    /// Coord returned 503 — the `work_unit_deps` table is not yet migrated.
    /// Benign: the JSONB fallback in `metadata.depends_on` covers dependencies
    /// until the migration lands.
    TableNotMigrated,
}

/// The coord side of a push, abstracted so [`push_work_unit`] is testable
/// without live HTTP. Implemented by [`HttpWorkUnitSink`] in production and a
/// fake in tests.
#[async_trait::async_trait]
pub trait WorkUnitSink: Send + Sync {
    /// Current opaque status of the unit, or `None` if it doesn't exist yet.
    ///
    /// **`Ok(None)` is a positive claim of absence, not a shrug.**
    /// [`super::trigger::backfill_work_units_once`] seeds `last_applied` from
    /// this, and a `None` seed routes the unit down `UpsertWithStatus` — the one
    /// arm that writes a status unconditionally and that the agent-owner
    /// deferral never gates. Any implementation that cannot *prove* the unit is
    /// absent (a truncated page, an envelope it does not recognize, a transport
    /// failure) must return `Err`, never `Ok(None)`.
    async fn current_status(&self, slug: &str) -> Result<Option<String>>;
    /// The `by_actor` of the unit's most-recent status-history row, or None if
    /// the unit has no history. Used to defer when a real (non-proxy) actor owns
    /// the unit. Reads GET /coord/agent-work-units/<slug>/history
    /// (newest-first) — the DEVICE-authed history door; see the module header
    /// for why the operator-tier twin is not usable from here.
    async fn last_actor(&self, slug: &str) -> Result<Option<String>>;
    async fn upsert(&self, body: &UpsertBody) -> Result<()>;
    async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()>;
    /// Replace the complete upstream dependency set of `slug` in coord's
    /// first-class edge table (`POST /coord/work-units/:slug/deps`). This is a
    /// REPLACE-SET: `depends_on` is the full upstream set; `&[]` clears all.
    /// Idempotent, so re-sending an unchanged set is harmless.
    async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome>;
}

/// Push one parsed work-unit through the edge-trigger + conflict logic.
///
/// `last_applied` is the status this adapter last applied for `u.slug` (its
/// client-side memory). Returns the [`PushOutcome`]; the caller updates its
/// last-applied memory to `u.status` on success.
pub async fn push_work_unit<S: WorkUnitSink + ?Sized>(
    sink: &S,
    u: &ParsedWorkUnit,
    last_applied: Option<&str>,
) -> Result<PushOutcome> {
    push_work_unit_with_remote(sink, u, last_applied, None).await
}

/// [`push_work_unit`] with the unit's remote status supplied by a caller that
/// has ALREADY read it.
///
/// `known_remote` is doubly optional on purpose: the outer `None` means "not
/// read — go and read it if you need it", and `Some(inner)` is a read result
/// where `inner` is the status (`None` = the unit does not exist).
///
/// This exists for [`super::trigger::backfill_work_units_once`], which seeds
/// `last_applied` FROM `current_status`. Without the hint the push would
/// immediately re-read the same value to run its conflict check against a
/// `prev` that IS that value — a comparison whose answer is fixed by
/// construction, bought with one extra HTTP GET per existing unit, i.e. roughly
/// double the read traffic of a ~1,400-plan catch-up.
pub async fn push_work_unit_with_remote<S: WorkUnitSink + ?Sized>(
    sink: &S,
    u: &ParsedWorkUnit,
    last_applied: Option<&str>,
    known_remote: Option<Option<&str>>,
) -> Result<PushOutcome> {
    let metadata = build_metadata(u);
    let mut action = decide_push(&u.status, last_applied);

    // The unit's remote status, read AT MOST ONCE per push and shared by the
    // deferral's convergence check and the conflict check. THREE states, not
    // two — see [`RemoteStatus`]. A caller-supplied `known_remote` is a
    // SUCCESSFUL read the caller already paid for, so it enters as `Known`;
    // there is no spelling of the hint that means "my read failed" (a caller
    // whose read failed must not push at all — that is exactly what
    // `backfill_work_units_once` does).
    let mut remote: RemoteStatus = match known_remote {
        Some(inner) => RemoteStatus::Known(inner.map(|s| s.to_string())),
        None => RemoteStatus::Unread,
    };

    // Deferral (graduation-bootstrap P2a): only a `Transition` would OVERWRITE
    // an existing unit's status. Before emitting it, read the unit's latest
    // status-history `by_actor`; if a real (non-adapter) actor last drove the
    // unit, a real agent owns its lifecycle now — DEFER (skip this cycle's
    // transition) so we don't collapse the agent's transition back to the system
    // actor. A brand-new unit (`UpsertWithStatus`) or an idempotent
    // `RefreshOnly` has no agent owner to defer to, so those are never gated.
    if let PushAction::Transition { .. } = &action {
        if let Some(actor) = sink.last_actor(&u.slug).await? {
            if is_real_agent_actor(&actor) {
                if matches!(remote, RemoteStatus::Unread) {
                    remote = read_remote_status(sink, &u.slug, "deferral convergence check").await;
                }
                // CONVERGENCE. The gate keys on ownership, but ownership alone
                // is not a reason to defer: if coord ALREADY holds the status
                // the file wants, there is nothing to overwrite and nothing to
                // protect. Deferring anyway would leave the caller's
                // last-applied memory permanently behind (it must not record a
                // status it did not apply), so the unit would re-enter this
                // branch on every future cycle — an HTTP read and a `deferred`
                // count, forever, for a unit the file and coord AGREE about.
                // Treat it as the plain refresh it is.
                //
                // Only a SUCCESSFUL read can establish convergence. An
                // UNREADABLE remote is UNKNOWN, and reading UNKNOWN as "coord
                // already holds what the file wants" would downgrade the
                // deferral to a refresh and let a later cycle transition over
                // an agent-owned unit on the strength of a 403. It falls
                // through to the deferral — the conservative arm, which writes
                // no status at all — and `read_remote_status` has already said
                // loudly why.
                let converged = matches!(&remote, RemoteStatus::Known(Some(s)) if s == &u.status);
                if converged {
                    action = PushAction::RefreshOnly;
                } else {
                    tracing::info!(
                        slug = %u.slug,
                        last_actor = %actor,
                        "markdown proxy defers: real agent owns this unit"
                    );
                    // Still refresh title/metadata — status-less, so it cannot
                    // touch what the agent set. The deferral suppresses the
                    // TRANSITION, not the provenance: skipping this too would
                    // freeze `source_path`, `phases` and `depends_on` for the
                    // whole life of the deferral.
                    sink.upsert(&UpsertBody {
                        slug: u.slug.clone(),
                        title: u.title.clone(),
                        status: None,
                        metadata: Some(metadata.clone()),
                        by_actor: Some(ADAPTER_ACTOR.to_string()),
                    })
                    .await?;
                    return Ok(PushOutcome {
                        slug: u.slug.clone(),
                        kind: PushOutcomeKind::Deferred {
                            owner: actor,
                            wanted: u.status.clone(),
                        },
                        conflict: false,
                    });
                }
            }
        }
    }

    // Conflict detection: did the remote status diverge from what we last
    // applied? (A direct transition by someone else.) File wins, but loudly.
    let mut conflict = false;
    if let Some(prev) = last_applied {
        if matches!(remote, RemoteStatus::Unread) {
            remote = read_remote_status(sink, &u.slug, "conflict check").await;
        }
        match &remote {
            RemoteStatus::Known(Some(remote_status)) => {
                if remote_status != prev {
                    conflict = true;
                    tracing::warn!(
                        slug = %u.slug,
                        last_applied = %prev,
                        remote = %remote_status,
                        parsed = %u.status,
                        "plan adapter: remote work-unit status diverged from last-applied; \
                         file wins (loud override)"
                    );
                }
            }
            // Provably absent (the `current_status` contract). There is no
            // remote status, so nothing can have diverged from `prev`.
            RemoteStatus::Known(None) => {}
            // UNKNOWN — and it gets its OWN warning rather than borrowing the
            // divergence one, because the two say opposite things: that one
            // reports a divergence we SAW, this one reports that we cannot
            // tell. `conflict` stays false, which KEEPS the CAS `from_status`
            // guard on any transition below — the conservative choice: if the
            // remote did move, coord refuses the transition instead of the
            // adapter overwriting a status it never read. `Unread` is folded in
            // here rather than given a silent arm: it cannot occur (the line
            // above resolves it) and it means the same thing if it ever does.
            RemoteStatus::Unreadable | RemoteStatus::Unread => {
                tracing::warn!(
                    slug = %u.slug,
                    last_applied = %prev,
                    parsed = %u.status,
                    "plan adapter: conflict verdict UNKNOWN — the remote work-unit status \
                     could not be read, so a divergence from last-applied can be neither \
                     confirmed nor ruled out; keeping the CAS from_status guard so coord \
                     refuses the transition if the remote moved"
                );
            }
        }
    }

    let kind = match &action {
        PushAction::UpsertWithStatus => {
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: Some(u.status.clone()),
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            PushOutcomeKind::Created
        }
        PushAction::RefreshOnly => {
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: None,
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            PushOutcomeKind::Refreshed
        }
        PushAction::Transition { from, to } => {
            // Refresh title/metadata first (no status change), then transition
            // so the history row carries the from->to edge.
            sink.upsert(&UpsertBody {
                slug: u.slug.clone(),
                title: u.title.clone(),
                status: None,
                metadata: Some(metadata),
                by_actor: Some(ADAPTER_ACTOR.to_string()),
            })
            .await?;
            sink.transition(
                &u.slug,
                &TransitionBody {
                    // On a detected conflict the CAS would fail (remote != from),
                    // so drop the guard and let the row's current status be the
                    // from_status — the file still wins.
                    from_status: if conflict { None } else { Some(from.clone()) },
                    to_status: to.clone(),
                    by_actor: ADAPTER_ACTOR.to_string(),
                    reason: Some(format!("plan file status edge: {from} -> {to}")),
                },
            )
            .await?;
            PushOutcomeKind::Transitioned {
                from: from.clone(),
                to: to.clone(),
            }
        }
    };

    Ok(PushOutcome {
        slug: u.slug.clone(),
        kind,
        conflict,
    })
}

/// Stamp `metadata.archive_path` for a plan found in the archive directory —
/// a **metadata-only** upsert (`status: None`, no transition, ever).
///
/// This is the D4 guard expressed in code: the archive scan is a metadata-only
/// writer. It records where a plan was archived to (`u.source_path`, the
/// archived file's filesystem path) as provenance and does **nothing else**. In
/// particular it never emits a status transition — not even when the archived
/// file's `> **Status:` line parses to `shipped` (coord-derived, not settable)
/// or the non-vocabulary `archived` (which coord would silently classify `Free`
/// and *accept*). Terminal state is owned by coord's derive engine; a second
/// writer racing it is exactly what this metadata-only path avoids.
///
/// A plan normally lives in exactly one directory (active *or* archive), so this
/// does not contend with the full-metadata upsert the active-dir reconcile
/// writes for the same slug.
pub async fn push_archive_metadata<S: WorkUnitSink + ?Sized>(
    sink: &S,
    u: &ParsedWorkUnit,
) -> Result<()> {
    sink.upsert(&UpsertBody {
        slug: u.slug.clone(),
        title: u.title.clone(),
        // NEVER a status write from the archive scan (D4).
        status: None,
        metadata: Some(serde_json::json!({ "archive_path": u.source_path })),
        by_actor: Some(ADAPTER_ACTOR.to_string()),
    })
    .await
}

/// Production [`WorkUnitSink`]: HTTP against coord with the device-JWT bearer.
pub struct HttpWorkUnitSink {
    base: String,
    client: reqwest::Client,
}

impl HttpWorkUnitSink {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Resolve from the runner's CONNECTED coord base
    /// ([`crate::profiles::connected_coord_base`] — env `COORD_HTTP_URL`, the
    /// active profile's `coord_url`, or the production default on a
    /// hosted-tier runner).
    ///
    /// `None` only when the runner is ISOLATED — i.e. genuinely standalone, so
    /// the trigger no-ops rather than dialing a dev-localhost guess. It is NOT
    /// "None when `coord_url` is absent": a `qontinui_account`-tier runner that
    /// never had a `coord_url` written into profiles.json is the SHIPPED
    /// end-user configuration, and reading it as unconfigured would silently
    /// drop the entire hosted fleet's work-unit state.
    pub fn from_profile() -> Option<Self> {
        crate::profiles::connected_coord_base().map(Self::new)
    }
}

#[async_trait::async_trait]
impl WorkUnitSink for HttpWorkUnitSink {
    async fn current_status(&self, slug: &str) -> Result<Option<String>> {
        // The query value is percent-encoded rather than inlined: the slug is a
        // filename stem and nothing upstream sanitises it, so an un-encoded `#`
        // or `&` would silently query for something else and the empty page
        // would read as a proven absence. (Hand-encoded rather than via
        // `RequestBuilder::query`, which is version-fragile in this tree.)
        let url = current_status_url(&self.base, slug);
        // coord-tenant-scope(work-owed): the periodic plan scan holds only self.base + self.client -- no session id exists in this module; the plan's repo is the only tenancy signal. Phase 6. (E4 is CLOSED for this call: the device-authed `get_list_agent` door lifts the tenant from the verified JWT, so a device JWT resolves here where the operator-tier `/coord/work-units` route 403s it.)
        let resp = crate::auth::attach_device_auth(self.client.get(&url))
            .send()
            .await
            .context("GET /coord/agent-work-units")?;
        if !resp.status().is_success() {
            return Err(classify_failure("GET /coord/agent-work-units", resp).await);
        }
        let body: serde_json::Value = resp.json().await.context("parse work-units list")?;
        status_from_list_body(&body, slug, PREFIX_SCAN_LIMIT)
    }

    async fn last_actor(&self, slug: &str) -> Result<Option<String>> {
        // GET /coord/agent-work-units/<slug>/history returns
        // {"work_unit_id":..,"slug":..,"history":[{..,"by_actor":..,"to_status":..,
        //  "transitioned_at":..}, ...]} ordered newest-first (coord's SQL
        // `ORDER BY transitioned_at DESC`) — the SAME `history_response` core
        // the operator door serves, reached with the device JWT this sink holds.
        let url = last_actor_url(&self.base, slug);
        // coord-tenant-scope(work-owed): same session-less sink; the slug is the only tenancy signal. Phase 6. (E4 is CLOSED for this call too: `get_history_agent` resolves the tenant from the verified device JWT, where the TenantId-gated /coord/work-units/:slug/history 403s it.)
        let resp = crate::auth::attach_device_auth(self.client.get(&url))
            .send()
            .await
            .context("GET /coord/agent-work-units/:slug/history")?;
        // No such unit yet ⇒ no history ⇒ no owner to defer to.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(classify_failure("GET /coord/agent-work-units/:slug/history", resp).await);
        }
        let body: serde_json::Value = resp.json().await.context("parse work-unit history")?;
        // Newest-first: the first `history` element is the most-recent transition.
        // `by_actor` is nullable (serialized as JSON null) — `as_str` yields None,
        // and an empty history array yields None, both meaning "no owner".
        let by_actor = body
            .get("history")
            .and_then(|h| h.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("by_actor"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(by_actor)
    }

    async fn upsert(&self, body: &UpsertBody) -> Result<()> {
        let url = format!("{}/coord/work-units/upsert", self.base);
        // coord-tenant-scope(work-owed): the headline site -- no session id in scope; coord's post_upsert lifts the tenant from the JWT claim and UpsertRequest has no tenant field, so the plan's repo must resolve it. Phase 6.
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(body))
            .send()
            .await
            .context("POST /coord/work-units/upsert")?;
        if !resp.status().is_success() {
            return Err(classify_failure("POST /coord/work-units/upsert", resp).await);
        }
        Ok(())
    }

    async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
        let url = format!("{}/coord/work-units/{}/transition", self.base, slug);
        // coord-tenant-scope(work-owed): same session-less sink; coord's post_transition uses the same tenant_from_claims(&auth), so the slug's repo is the only tenancy signal. Phase 6.
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(body))
            .send()
            .await
            .context("POST /coord/work-units/:slug/transition")?;
        if !resp.status().is_success() {
            return Err(classify_failure("POST /coord/work-units/:slug/transition", resp).await);
        }
        Ok(())
    }

    async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
        let url = format!("{}/coord/work-units/{}/deps", self.base, slug);
        let body = serde_json::json!({ "depends_on": depends_on });
        // coord-tenant-scope(work-owed): same session-less sink; the deps route is tenant-scoped fail-closed off the JWT, so the plan's repo must supply the tenant. Phase 6.
        let resp = crate::auth::attach_device_auth(self.client.post(&url).json(&body))
            .send()
            .await
            .context("POST /coord/work-units/:slug/deps")?;
        let status = resp.status();
        // 503 => the edge table hasn't been migrated yet; benign, the JSONB
        // fallback in metadata.depends_on covers it.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(SetDepsOutcome::TableNotMigrated);
        }
        if !status.is_success() {
            return Err(classify_failure("POST /coord/work-units/:slug/deps", resp).await);
        }
        let parsed: serde_json::Value = resp.json().await.unwrap_or_default();
        let edges_set = parsed
            .get("edges_set")
            .and_then(|v| v.as_u64())
            .unwrap_or(depends_on.len() as u64);
        Ok(SetDepsOutcome::Ok { edges_set })
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::{ParsedPhase, ParsedWorkUnit};
    use super::*;
    use std::sync::Mutex;

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: Some("T".to_string()),
            status: status.to_string(),
            depends_on: vec!["2026-01-01-dep".to_string()],
            phases: vec![ParsedPhase {
                index: 1,
                name: "Phase 1 — x".to_string(),
            }],
            source_path: format!("plans/{slug}.md"),
            content: String::new(),
        }
    }

    /// Regression guard (Phase 4): the archive scan must not weaken the
    /// second-writer deference. `is_real_agent_actor` classifies exactly the
    /// adapter's own actor and the empty actor as "not a real owner"; anything
    /// else is a real agent owner the proxy defers to.
    #[test]
    fn is_real_agent_actor_deference_unchanged() {
        assert!(!is_real_agent_actor(ADAPTER_ACTOR));
        assert!(!is_real_agent_actor(""));
        assert!(is_real_agent_actor("device:d:agent:a"));
        assert!(is_real_agent_actor("some-other-system-actor"));
    }

    #[tokio::test]
    async fn push_archive_metadata_stamps_path_and_never_transitions() {
        let sink = FakeSink::default();
        // A shipped archived plan: the archive scan must NOT transition it —
        // it only stamps provenance.
        let u = unit("2026-01-01-done", "shipped");
        push_archive_metadata(&sink, &u).await.unwrap();

        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1, "exactly one metadata-only upsert");
        assert!(ups[0].status.is_none(), "archive upsert carries NO status");
        assert_eq!(
            ups[0].metadata.as_ref().unwrap()["archive_path"],
            serde_json::json!(u.source_path),
            "archive_path stamped to the archived file path"
        );
        assert!(
            sink.transitions.lock().unwrap().is_empty(),
            "archive scan NEVER transitions"
        );
    }

    /// The non-vocabulary `archived` status classifies `Free` on coord and is
    /// silently accepted — so the client-side no-transition guard is the only
    /// thing stopping a second write. Assert it holds for `archived` too.
    #[tokio::test]
    async fn push_archive_metadata_no_transition_even_for_archived_status() {
        let sink = FakeSink::default();
        let u = unit("2026-01-02-old", "archived");
        push_archive_metadata(&sink, &u).await.unwrap();
        assert!(sink.transitions.lock().unwrap().is_empty());
        assert!(sink.upserts.lock().unwrap()[0].status.is_none());
    }

    // ---- `status_from_list_body`: the absence proof, without HTTP ----------

    /// A row that EXISTS answers with its status — including the empty-string
    /// seed and a JSON-null status, neither of which may read as "absent".
    #[test]
    fn status_from_list_body_reads_a_present_row() {
        let page = |rows: serde_json::Value| serde_json::json!({"work_units": rows, "limit": 500});
        assert_eq!(
            status_from_list_body(
                &page(serde_json::json!([{"slug":"s","status":"draft"}])),
                "s",
                500
            )
            .unwrap(),
            Some("draft".to_string())
        );
        assert_eq!(
            status_from_list_body(
                &page(serde_json::json!([{"slug":"s","status":""}])),
                "s",
                500
            )
            .unwrap(),
            Some(String::new()),
            "the empty-string seed is a PRESENT unit with no status"
        );
        assert_eq!(
            status_from_list_body(
                &page(serde_json::json!([{"slug":"s","status":null}])),
                "s",
                500
            )
            .unwrap(),
            Some(String::new()),
            "a null status on a present row is NOT an absent unit"
        );
        // The bare-array envelope is accepted too.
        assert_eq!(
            status_from_list_body(
                &serde_json::json!([{"slug":"s","status":"vetted"}]),
                "s",
                500
            )
            .unwrap(),
            Some("vetted".to_string())
        );
    }

    /// A short page with no match is the ONLY shape that proves absence.
    #[test]
    fn status_from_list_body_proves_absence_only_on_a_short_page() {
        let body =
            serde_json::json!({"work_units": [{"slug":"other","status":"draft"}], "limit": 500});
        assert_eq!(status_from_list_body(&body, "s", 500).unwrap(), None);
    }

    /// A FULL page with no match is a truncated scan — UNKNOWN, not absent.
    /// Neuter check: drop the `rows.len() >= applied_limit` guard and this
    /// fails, and with it the promise that `Ok(None)` licenses the
    /// unconditional status write.
    #[test]
    fn status_from_list_body_refuses_to_call_a_truncated_page_absent() {
        let rows: Vec<serde_json::Value> = (0..3)
            .map(|i| serde_json::json!({"slug": format!("other-{i}"), "status": "draft"}))
            .collect();
        let body = serde_json::json!({"work_units": rows, "limit": 3});
        let err = status_from_list_body(&body, "s", 500).unwrap_err();
        assert!(format!("{err}").contains("UNKNOWN"), "got {err}");
        assert!(
            format!("{err}").contains("limit 3"),
            "the guard must use the limit coord APPLIED, not the one we asked for: {err}"
        );
    }

    /// An envelope the reader does not understand is an error, never zero rows.
    #[test]
    fn status_from_list_body_refuses_an_unrecognized_envelope() {
        for body in [
            serde_json::json!(null),
            serde_json::json!("nope"),
            serde_json::json!(7),
            serde_json::json!({"detail": "forbidden"}),
            serde_json::json!({"work_units": "not-an-array"}),
        ] {
            assert!(
                status_from_list_body(&body, "s", 500).is_err(),
                "an unreadable body must be UNKNOWN, not an absent unit: {body}"
            );
        }
    }

    /// The slug reaches the query string encoded — an un-encoded `&` or `#`
    /// would query for something else entirely and the empty page would read as
    /// a proven absence.
    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(
            percent_encode_query_value("2026-01-01-plan_a.b~c"),
            "2026-01-01-plan_a.b~c",
            "the unreserved set passes through untouched"
        );
        assert_eq!(percent_encode_query_value("a&b=c#d e"), "a%26b%3Dc%23d%20e");
        assert_eq!(percent_encode_query_value("100%"), "100%25");
    }

    #[test]
    fn decide_push_edge_trigger() {
        assert_eq!(decide_push("vetted", None), PushAction::UpsertWithStatus);
        assert_eq!(
            decide_push("vetted", Some("vetted")),
            PushAction::RefreshOnly
        );
        assert_eq!(
            decide_push("shipped", Some("vetted")),
            PushAction::Transition {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
    }

    #[test]
    fn build_metadata_carries_enrichment() {
        let m = build_metadata(&unit("s", "vetted"));
        assert_eq!(m["depends_on"][0], "2026-01-01-dep");
        assert_eq!(m["phases"][0]["index"], 1);
        assert_eq!(m["source_path"], "plans/s.md");
    }

    #[test]
    fn upsert_body_omits_none_status() {
        let b = UpsertBody {
            slug: "s".to_string(),
            title: None,
            status: None,
            metadata: None,
            by_actor: None,
        };
        let j = serde_json::to_value(&b).unwrap();
        assert_eq!(j, serde_json::json!({"slug": "s"}));
    }

    #[derive(Default)]
    struct FakeSink {
        remote: Option<String>,
        /// When set, `current_status` returns `Err` — the shape a 403, a
        /// transport failure or an unreadable envelope all take. The whole
        /// point of [`RemoteStatus`] is that this must never be consumable as
        /// `Ok(None)`, so the fake has to be able to produce it.
        status_err: bool,
        /// How many times `current_status` was called — pins that a supplied
        /// remote hint actually spares the read.
        status_reads: Mutex<u32>,
        /// Configured `by_actor` of the unit's latest history row (default None).
        last_actor: Option<String>,
        upserts: Mutex<Vec<UpsertBody>>,
        transitions: Mutex<Vec<(String, TransitionBody)>>,
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, _slug: &str) -> Result<Option<String>> {
            *self.status_reads.lock().unwrap() += 1;
            if self.status_err {
                anyhow::bail!("GET /coord/agent-work-units returned 403 Forbidden");
            }
            Ok(self.remote.clone())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            self.upserts.lock().unwrap().push(body.clone());
            Ok(())
        }
        async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
            self.transitions
                .lock()
                .unwrap()
                .push((slug.to_string(), body.clone()));
            Ok(())
        }
        async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
            self.deps_calls
                .lock()
                .unwrap()
                .push((slug.to_string(), depends_on.to_vec()));
            Ok(SetDepsOutcome::Ok {
                edges_set: depends_on.len() as u64,
            })
        }
    }

    #[tokio::test]
    async fn fake_sink_set_deps_records_call_and_returns_ok() {
        let sink = FakeSink::default();
        let out = sink
            .set_deps("p4", &["p1".to_string(), "p2".to_string()])
            .await
            .unwrap();
        assert_eq!(out, SetDepsOutcome::Ok { edges_set: 2 });
        let calls = sink.deps_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "p4");
        assert_eq!(calls[0].1, vec!["p1".to_string(), "p2".to_string()]);
    }

    #[tokio::test]
    async fn first_push_creates_with_status_no_transition() {
        let sink = FakeSink::default();
        let out = push_work_unit(&sink, &unit("s", "vetted"), None)
            .await
            .unwrap();
        assert_eq!(out.kind, PushOutcomeKind::Created);
        assert!(!out.conflict);
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].status.as_deref(), Some("vetted"));
        assert!(sink.transitions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unchanged_status_refreshes_only_no_transition() {
        let sink = FakeSink {
            remote: Some("vetted".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "vetted"), Some("vetted"))
            .await
            .unwrap();
        assert_eq!(out.kind, PushOutcomeKind::Refreshed);
        assert!(!out.conflict);
        // Refresh upsert carries NO status (doesn't clobber), no transition.
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1);
        assert!(ups[0].status.is_none());
        assert!(sink.transitions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_edge_emits_transition_with_cas() {
        let sink = FakeSink {
            remote: Some("vetted".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert_eq!(
            out.kind,
            PushOutcomeKind::Transitioned {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
        assert!(!out.conflict);
        let trs = sink.transitions.lock().unwrap();
        assert_eq!(trs.len(), 1);
        assert_eq!(trs[0].1.from_status.as_deref(), Some("vetted")); // CAS guard set
        assert_eq!(trs[0].1.to_status, "shipped");
    }

    /// A deferral must still refresh title/metadata — status-less, so it cannot
    /// touch what the agent set. Skipping the upsert entirely (the shipped
    /// behaviour) froze `source_path`, `phases` and `depends_on` for the whole
    /// life of the deferral.
    #[tokio::test]
    async fn a_deferral_still_refreshes_metadata_but_never_the_status() {
        let sink = FakeSink {
            remote: Some("shipped".to_string()),
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "in_progress"), Some("vetted"))
            .await
            .unwrap();
        assert!(matches!(out.kind, PushOutcomeKind::Deferred { .. }));
        assert!(sink.transitions.lock().unwrap().is_empty());
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1, "provenance is still refreshed");
        assert!(ups[0].status.is_none(), "…with NO status");
        assert_eq!(
            ups[0].metadata.as_ref().unwrap()["source_path"],
            "plans/s.md"
        );
    }

    /// Ownership alone is not a reason to defer. When coord ALREADY holds the
    /// status the file wants there is nothing to overwrite, so the push settles
    /// as a refresh — otherwise the caller could never advance its last-applied
    /// memory (it must not record a status it did not apply) and the unit would
    /// re-enter the deferral branch, and be re-counted, on every future cycle.
    #[tokio::test]
    async fn no_deferral_when_the_agent_already_set_the_status_the_file_wants() {
        let sink = FakeSink {
            remote: Some("shipped".to_string()),
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert_eq!(out.kind, PushOutcomeKind::Refreshed);
        assert!(
            sink.transitions.lock().unwrap().is_empty(),
            "and it still never transitions an agent-owned unit"
        );
    }

    /// The caller-supplied remote hint spares the redundant read.
    #[tokio::test]
    async fn a_supplied_remote_hint_is_used_instead_of_re_reading() {
        let sink = FakeSink {
            remote: Some("vetted".to_string()),
            ..Default::default()
        };
        let out = push_work_unit_with_remote(
            &sink,
            &unit("s", "shipped"),
            Some("vetted"),
            Some(Some("vetted")),
        )
        .await
        .unwrap();
        assert!(!out.conflict);
        assert_eq!(
            *sink.status_reads.lock().unwrap(),
            0,
            "the hint means current_status is never called"
        );
    }

    #[tokio::test]
    async fn remote_divergence_is_conflict_and_drops_cas() {
        // We last applied "vetted", but remote is "in_progress" (someone moved
        // it). File says "shipped": conflict surfaced, file wins, CAS dropped.
        let sink = FakeSink {
            remote: Some("in_progress".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert!(out.conflict);
        assert_eq!(
            out.kind,
            PushOutcomeKind::Transitioned {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
        let trs = sink.transitions.lock().unwrap();
        assert_eq!(trs[0].1.from_status, None); // CAS dropped on conflict
        assert_eq!(trs[0].1.to_status, "shipped");
    }

    // ---- UNKNOWN is a THIRD state, not a spelling of "absent" -------------

    /// The modelling itself: a failed read and a proven absence land in
    /// DIFFERENT states. Neuter check — collapse `Err` back into
    /// `Known(None)` (the `.ok().flatten()` this phase deleted) and this fails.
    #[tokio::test]
    async fn a_failed_status_read_is_unreadable_never_a_proven_absence() {
        let failing = FakeSink {
            status_err: true,
            ..Default::default()
        };
        assert_eq!(
            read_remote_status(&failing, "s", "test").await,
            RemoteStatus::Unreadable
        );
        let absent = FakeSink::default();
        assert_eq!(
            read_remote_status(&absent, "s", "test").await,
            RemoteStatus::Known(None)
        );
        assert_ne!(RemoteStatus::Unreadable, RemoteStatus::Known(None));
    }

    /// A `current_status` `Err` in the CONFLICT CHECK must not be consumed as
    /// "no divergence". It still does not SET `conflict` — we observed no
    /// divergence and must not claim one — but it takes the UNKNOWN arm (which
    /// warns) and the CAS `from_status` guard is KEPT deliberately, so coord
    /// refuses the transition if the remote did move. Before Phase 0 this case
    /// was byte-identical to a clean read of an agreeing remote, and emitted no
    /// log line at all.
    #[tokio::test]
    async fn an_unreadable_remote_is_not_silently_no_divergence() {
        let sink = FakeSink {
            status_err: true,
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert!(
            !out.conflict,
            "an unread remote is UNKNOWN, never a POSITIVE conflict claim"
        );
        assert_eq!(
            *sink.status_reads.lock().unwrap(),
            1,
            "the read was attempted exactly once"
        );
        let trs = sink.transitions.lock().unwrap();
        assert_eq!(
            trs[0].1.from_status.as_deref(),
            Some("vetted"),
            "CAS guard KEPT on an unknown remote, so coord refuses if it moved"
        );
    }

    /// A `current_status` `Err` in the DEFERRAL arm must not be read as
    /// convergence. Contrast
    /// `no_deferral_when_the_agent_already_set_the_status_the_file_wants`:
    /// there the read SUCCEEDS and agrees, so the deferral downgrades to a
    /// refresh. Here the read fails, so "coord already holds what the file
    /// wants" is unknown — and treating unknown as convergence would let a
    /// later cycle transition over an agent-owned unit on the strength of a
    /// 403. It falls through to the deferral, which writes no status at all.
    #[tokio::test]
    async fn an_unreadable_remote_is_never_read_as_convergence() {
        let sink = FakeSink {
            status_err: true,
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert!(
            matches!(out.kind, PushOutcomeKind::Deferred { .. }),
            "UNKNOWN falls through to the deferral, not to the refresh: {:?}",
            out.kind
        );
        assert!(
            sink.transitions.lock().unwrap().is_empty(),
            "an agent-owned unit is never transitioned on an unreadable remote"
        );
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 1, "the deferral still refreshes provenance");
        assert!(ups[0].status.is_none(), "…status-less, as before");
    }

    /// `Ok(None)` — a PROVEN absence — is unchanged by this phase: no conflict,
    /// no UNKNOWN arm, and the CAS guard kept, exactly as before.
    #[tokio::test]
    async fn a_provably_absent_unit_is_still_not_a_conflict() {
        let sink = FakeSink::default();
        let out = push_work_unit(&sink, &unit("s", "shipped"), Some("vetted"))
            .await
            .unwrap();
        assert!(!out.conflict);
        assert_eq!(
            out.kind,
            PushOutcomeKind::Transitioned {
                from: "vetted".to_string(),
                to: "shipped".to_string()
            }
        );
        assert_eq!(
            sink.transitions.lock().unwrap()[0].1.from_status.as_deref(),
            Some("vetted")
        );
    }

    // ---- the doors themselves --------------------------------------------

    /// Both READS must target the DEVICE-authed `agent-work-units` doors. The
    /// operator-tier `/coord/work-units*` twins are `TenantId`-gated and 403 a
    /// device JWT — measured at 380–1,058 `history returned 403` lines per
    /// window, with the deferral guard never once evaluating its predicate.
    #[test]
    fn both_reads_target_the_device_authed_agent_doors() {
        assert_eq!(
            current_status_url("https://coord.example.test", "2026-01-01-plan"),
            "https://coord.example.test/coord/agent-work-units\
             ?slug_prefix=2026-01-01-plan&limit=500"
        );
        assert_eq!(
            last_actor_url("https://coord.example.test", "2026-01-01-plan"),
            "https://coord.example.test/coord/agent-work-units/2026-01-01-plan/history"
        );
        for url in [
            current_status_url("https://c", "s"),
            last_actor_url("https://c", "s"),
        ] {
            assert!(url.contains("/coord/agent-work-units"), "{url}");
            assert!(
                !url.contains("/coord/work-units"),
                "the operator-tier door 403s the device JWT this sink holds: {url}"
            );
        }
        // The list door still gets an ENCODED query value — an un-encoded `&`
        // would query for something else and its empty page would read as a
        // proven absence.
        assert!(current_status_url("https://c", "a&b").contains("slug_prefix=a%26b"));
    }
}
