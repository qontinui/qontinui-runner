//! The six outbound data flows and the per-tenant switch that governs each one,
//! enforced at the SOURCE — before any byte leaves this machine.
//!
//! Plan `2026-10-10-spec-front-end-phase-9-generic-boundary`, Phase 7 (design
//! decisions C4–C7).
//!
//! ## The flows
//!
//! | [`Flow`] | coord domain | what leaves, and where to |
//! |---|---|---|
//! | `TranscriptSync` | `egress_transcript_sync` | AI session transcripts, tenant memory records and memory queries, to coord / qontinui-web |
//! | `CodeMirror` | `egress_code_mirror` | agent branches, `git push`ed to coord's git origin |
//! | `TerminalStream` | `egress_terminal_stream` | raw terminal (PTY) output to coord and through the web relay, remote terminal attach, AND the relay's AI-session content (`ai-output` / `session-state` forwards, `chat_get_output`) |
//! | `Telemetry` | `egress_telemetry` | crash reports (Sentry), OTLP spans, and the relay's `ui-error` / `recent-crash` forwards |
//! | `UpdateCheck` | `egress_update_check` | the updater's request for the latest release manifest |
//! | `SkillMirror` | `egress_skill_mirror` | the `git fetch` of the canonical skill/command corpus |
//!
//! The relay's AI-session channels ride `TerminalStream`, not `TranscriptSync`:
//! they are a LIVE view of a session streamed to the web/mobile console (the
//! same audience and transport as a terminal pane), not the durable transcript
//! archive transcript sync writes to coord.
//!
//! Each flow is one coord fleet-policy domain with levels `on` | `off`, tenant
//! band, ON by default (C4). The domain list is VENDORED here as
//! [`EGRESS_DOMAINS`], the way `fleet_policy_poller` vendors coord's other
//! domain strings, and a drift test compares it against coord's own
//! `EGRESS_DOMAINS` when a sibling coord checkout declares it.
//!
//! ## Resolution (C6)
//!
//! [`permit`] answers, for one flow, from the first rung that has a value:
//!
//! 1. [`LevelSource::Coord`] — the last AUTHORITATIVE answer coord gave THIS
//!    process (written by `mcp::fleet_policy_poller`);
//! 2. [`LevelSource::Persisted`] — the authoritative answer a previous process
//!    received, restored from `<config_dir>/egress-levels.json`, so the
//!    boot-time flows (crash reporting, OTLP, the startup update check) obey a
//!    tenant `off` before the first poll;
//! 3. [`LevelSource::Profile`] — the machine profile's `egress.default`
//!    (`~/.qontinui/profiles.json`), the self-hosted deployment's own switch
//!    (C7). A profiles.json that is PRESENT but unreadable, or names an active
//!    profile it does not define, reads `off`: only an absent file is "no
//!    opinion";
//! 4. [`LevelSource::ProductDefault`] — `on`.
//!
//! Every verdict carries the rung that produced it — and, for the two coord
//! rungs, whether a tenant row or the deployment's profile decided it — so a
//! fallback never poses as a tenant decision
//! ([policy: unknown-must-not-render-as-a-default]).
//!
//! ## What counts as an authoritative coord answer
//!
//! Only two answers are decisions ([`classify_coord_answer`]):
//!
//! - an explicit row (`resolved_scope` other than `none`) — the tenant chose;
//! - a no-row answer with `default_source: "deployment_profile"` — a
//!   self-hosted coord's egress `off`.
//!
//! A no-row answer with `default_source: "product"` is coord having NO opinion
//! for this tenant ([`CoordAnswer::NoOpinion`]): it replaces any decision held
//! for that scope (a deleted tenant row must not live on in the store) and
//! lets the machine profile, then the product default, answer — it never
//! outranks a profile's `off`. A no-row answer with no `default_source` at all
//! comes from a coord that predates the egress family
//! ([`CoordAnswer::NotAnEgressAnswer`]): it marks the scope answered (no
//! longer UNKNOWN) without overriding anything already held. A FAILED fetch
//! (no credential, 401, 404, network, 5xx) is no answer at all: nothing is
//! recorded or persisted, the scope keeps whatever it held, and an unanswered
//! tenant stays UNKNOWN.
//!
//! ## Scope: per coord deployment and per tenant; UNKNOWN fails closed
//!
//! Answers are keyed by [`ScopeKey`] — the coord base URL plus the tenant —
//! both in memory and in the store, so a runner re-pointed at another coord,
//! or re-paired into another tenant, never inherits the previous one's
//! switches. The poller asks coord once per tenant this device holds a
//! credential for, presenting THAT tenant's credential, and files each answer
//! under the tenant the presented credential names. A call site that knows
//! its session's tenant asks [`permit_for`] with it; everything else asks in
//! the default scope, whose tenant is the one a new session is stamped with
//! (`session::resolve_new_session_tenant`), so the two agree. A move of the
//! default scope (re-pairing, a new coord) takes effect once coord has
//! answered for the new scope; until then the stricter of the two applies.
//!
//! A scope that names a tenant but holds no answer from a SUCCESSFUL coord
//! reply — this process's or a persisted one — is UNKNOWN
//! ([`LevelSource::Unknown`]) and every flow fails closed there, never the
//! product default; a failed poll does not change that. A device with no
//! tenant at all has no tenant policy to wait for, and the profile, then the
//! product default, answer.
//!
//! A session-keyed CONTENT send (terminal / AI output, transcripts, session
//! state) whose tenant cannot be established ([`SessionScope::Unresolved`])
//! is judged by the STRICTEST verdict across the default scope and every
//! bound tenant ([`permit_session`]): any refusal refuses. The bound set is
//! every tenant the poller has enumerated, MERGED across ticks (a failed or
//! partial enumeration never shrinks it), seeded at start from the tenants
//! the store holds answers for under the current coord. An EMPTY set refuses
//! such a send, unless a complete enumeration found the device holds no
//! credentials at all.
//!
//! Which paths know their session's tenant:
//!
//! - the session-output drain (the record's owning session), the PTY output
//!   pipe (the session's scope), transcript-bind (the caller's tenant), the
//!   transcript tailer / watcher and emitter (the tenant the registrar
//!   recorded for the session), and the code mirror (the agent token's
//!   `tenant_id` claim) — all ask in that tenant;
//! - the web relay (`terminal_*`, `chat_get_output`, the `http_request`
//!   task-run reads and the `terminal-*` / `ai-output` / `session-state`
//!   forwards) and the target side of remote terminal attach ask in the tenant
//!   of the session the frame names — the open session on its terminal
//!   ([`terminal_session_tenant`]) or the registrar's record for its task run
//!   ([`task_run_session_tenant`]); the content fields of task-run, findings
//!   and pending-question reads over `http_request` are blanked per record
//!   in that record's run's tenant;
//! - what cannot be attributed is judged by the strictest bound tenant: a
//!   frame or event naming no session or one this runner does not know, a
//!   lifecycle stamp that is not a UUID, a run not registered since a restart,
//!   an `http_request` read of `/processes/{id}/output` (a process id names no
//!   session), and the SOURCE side of remote attach (the terminal lives on
//!   another device);
//! - the relay's `ui-error` / `recent-crash` forwards are telemetry,
//!   device-wide, in the default scope;
//! - telemetry, the update check and the skill mirror are device-wide and ask
//!   in the default scope.
//!
//! ## Where each switch is enforced
//!
//! The check sits at the point where the flow's bytes would leave — never on
//! ingest. The callers, one per flow:
//!
//! - transcript sync: [`transcript_sync_permitted`] / [`transcript_sync_gate_for`],
//!   replacing every egress-path read of the user's `cloud_sync_enabled`
//!   (getter or field), plus the outbox drain
//!   (`session::coord_sync::push_record`) as the last line;
//! - terminal stream: `session::output_pipe::flush`, the drain, the relay's
//!   `terminal_*` / `chat_get_output` handlers and its `terminal-output` /
//!   `ai-output` / `session-state` forwards,
//!   `mcp::remote_terminal::gate_remote_frame` and `RemoteAttachClient`;
//! - code mirror: `agent_pusher::push_one`, and the relay's `http_request`
//!   reads of repo / worktree content (`/files/read`, `/files/browse`,
//!   `/worktrees/diff`) in the tenant of the session working in that path's
//!   closest enclosing directory ([`path_session_tenant`]), else the
//!   strictest bound tenant;
//! - telemetry: the `sentry::init` block in `main`, `otel::init_otel`, and the
//!   relay's `ui-error` / `recent-crash` forwards;
//! - update check: `check_for_updates` / `install_update`;
//! - skill mirror: `canonical_corpus::refresh_into`.
//!
//! `GET /health` carries `egress` ([`health_json`]): the polled scope, and
//! every flow's verdict, its source, and how many sends the switch refused in
//! this process.
//!
//! The switch is a preference, never the safeguard (D3): authorisation stays
//! where it already is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

/// The six coord fleet-policy domains of the egress family, VENDORED from
/// `qontinui-coord/crates/coord/src/fleet_policy.rs` `EGRESS_DOMAINS`.
///
/// Order is [`Flow::ALL`]'s order. `vendored_domains_match_coord` reads coord's
/// copy through the sibling checkout and fails on any difference in members.
pub(crate) const EGRESS_DOMAINS: [&str; 6] = [
    "egress_transcript_sync",
    "egress_code_mirror",
    "egress_terminal_stream",
    "egress_telemetry",
    "egress_update_check",
    "egress_skill_mirror",
];

/// File name of the persisted-answer store, under the per-instance config dir.
const STORE_FILE: &str = "egress-levels.json";

/// Store schema version. A schema-1 file is migrated (its `off` values, into
/// the default scope); any other value is treated as absent.
const STORE_SCHEMA: u32 = 2;

/// The `resolved_scope` coord answers when no policy row exists.
const RESOLVED_SCOPE_NONE: &str = "none";

/// The `default_source` of a self-hosted coord's egress default.
const DEFAULT_SOURCE_DEPLOYMENT_PROFILE: &str = "deployment_profile";

/// The `default_source` of coord's product default — coord has no opinion.
const DEFAULT_SOURCE_PRODUCT: &str = "product";

/// What `/health` says about how scopes are polled and judged.
const SCOPE_NOTE: &str = "every tenant this device holds a credential for is polled with that \
     credential; a failed poll records nothing, so a tenant with no answer from a successful \
     coord reply (this process's or persisted) is UNKNOWN and every flow fails closed for it; a \
     content send whose session tenant cannot be established takes the strictest verdict across \
     all bound tenants (merged across enumerations, never shrunk by a failed one), and refuses \
     while that set is empty unless a complete enumeration found no credentials; a device with \
     no tenant at all uses the machine profile, else the product default";

/// One outbound data flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Flow {
    TranscriptSync,
    CodeMirror,
    TerminalStream,
    Telemetry,
    UpdateCheck,
    SkillMirror,
}

impl Flow {
    /// Every flow, in [`EGRESS_DOMAINS`] order.
    pub(crate) const ALL: [Flow; 6] = [
        Flow::TranscriptSync,
        Flow::CodeMirror,
        Flow::TerminalStream,
        Flow::Telemetry,
        Flow::UpdateCheck,
        Flow::SkillMirror,
    ];

    /// Position in [`Flow::ALL`] / [`EGRESS_DOMAINS`].
    const fn index(self) -> usize {
        match self {
            Flow::TranscriptSync => 0,
            Flow::CodeMirror => 1,
            Flow::TerminalStream => 2,
            Flow::Telemetry => 3,
            Flow::UpdateCheck => 4,
            Flow::SkillMirror => 5,
        }
    }

    /// The coord fleet-policy domain carrying this flow's switch.
    pub(crate) const fn domain(self) -> &'static str {
        EGRESS_DOMAINS[self.index()]
    }

    /// The flow's key in `/health` `egress` and in refusal frames
    /// (`transcript_sync`, `code_mirror`, …): the domain without its prefix.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Flow::TranscriptSync => "transcript_sync",
            Flow::CodeMirror => "code_mirror",
            Flow::TerminalStream => "terminal_stream",
            Flow::Telemetry => "telemetry",
            Flow::UpdateCheck => "update_check",
            Flow::SkillMirror => "skill_mirror",
        }
    }

    /// Whether a flip of this flow takes effect only at the runner's NEXT start.
    ///
    /// Only telemetry: crash reporting and the OTLP exporter are installed once
    /// at boot. The relay's `ui-error` / `recent-crash` forwards — also
    /// telemetry — re-check live, but the flow as a whole is honest only as
    /// "applies at next start". Every other flow re-checks on each send.
    pub(crate) const fn applies_at_next_start(self) -> bool {
        matches!(self, Flow::Telemetry)
    }

    /// The flow whose domain is `domain`, if any.
    pub(crate) fn from_domain(domain: &str) -> Option<Flow> {
        Flow::ALL.into_iter().find(|f| f.domain() == domain)
    }
}

/// A flow's level: the two values of the domain's tenant-band vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    On,
    Off,
}

impl Level {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Level::On => "on",
            Level::Off => "off",
        }
    }

    /// Exactly `on` / `off`, trimmed and case-insensitive. Anything else is
    /// `None` — callers decide what an unreadable level means for them.
    fn parse(raw: &str) -> Option<Level> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on" => Some(Level::On),
            "off" => Some(Level::Off),
            _ => None,
        }
    }
}

/// What, on coord's side, decided an authoritative answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoordOrigin {
    /// An explicit tenant-band row — the tenant chose.
    TenantRow,
    /// No row; a self-hosted coord's `COORD_DEPLOYMENT_PROFILE` default.
    DeploymentProfile,
    /// An `off` carried over from a schema-1 store, which did not record what
    /// decided it. Kept (fail-closed) until coord answers for the scope.
    LegacyStore,
}

impl CoordOrigin {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            CoordOrigin::TenantRow => "tenant_row",
            CoordOrigin::DeploymentProfile => "deployment_profile",
            CoordOrigin::LegacyStore => "legacy_store",
        }
    }
}

/// One authoritative coord decision for one flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Decision {
    pub(crate) level: Level,
    pub(crate) decided_by: CoordOrigin,
}

/// What coord has said about one flow in one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// A decision (rungs 1/2).
    Decided(Decision),
    /// Coord answered and holds no decision for this tenant — its product
    /// default, or a coord that predates the egress family. The machine
    /// profile, then the product default, answer.
    NoOpinion,
}

/// Which C6 rung produced a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelSource {
    Coord(CoordOrigin),
    Persisted(CoordOrigin),
    Profile,
    ProductDefault,
    /// The scope names a tenant, and neither this process nor the store holds
    /// any answer for it: the tenant's switch is UNKNOWN, and the flow fails
    /// closed rather than assuming the product default.
    Unknown,
}

impl LevelSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            LevelSource::Coord(_) => "coord",
            LevelSource::Persisted(_) => "persisted",
            LevelSource::Profile => "profile",
            LevelSource::ProductDefault => "product_default",
            LevelSource::Unknown => "unknown",
        }
    }

    /// What on coord's side decided it, for the two coord rungs.
    pub(crate) const fn decided_by(self) -> Option<CoordOrigin> {
        match self {
            LevelSource::Coord(o) | LevelSource::Persisted(o) => Some(o),
            LevelSource::Profile | LevelSource::ProductDefault | LevelSource::Unknown => None,
        }
    }
}

/// What [`permit`] answers for one flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EgressVerdict {
    pub(crate) allowed: bool,
    pub(crate) source: LevelSource,
}

/// Which tenant a SESSION-keyed send belongs to, as far as the call site could
/// establish it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionScope {
    /// The session's tenant is known.
    Tenant(Uuid),
    /// The session was positively established as the device default's (spawned
    /// without a tenant choice, or a route with no tenant dimension).
    DeviceDefault,
    /// The tenant could not be established: no session record, an
    /// unregistered run after a restart, a stamp that is not a UUID, a frame
    /// naming no session. A CONTENT flow is then judged by the STRICTEST
    /// verdict across every tenant this device is bound to — never the default
    /// scope alone, which says nothing about whose content it is.
    Unresolved,
}

impl From<crate::auth::TenantScope> for SessionScope {
    fn from(scope: crate::auth::TenantScope) -> Self {
        match scope {
            crate::auth::TenantScope::Owned(t) => SessionScope::Tenant(t),
            crate::auth::TenantScope::Device => SessionScope::DeviceDefault,
            crate::auth::TenantScope::Unresolved => SessionScope::Unresolved,
        }
    }
}

impl SessionScope {
    /// A lookup that answers `Some(tenant)` or nothing: nothing is
    /// [`SessionScope::Unresolved`].
    pub(crate) fn from_lookup(tenant: Option<Uuid>) -> Self {
        tenant.map_or(SessionScope::Unresolved, SessionScope::Tenant)
    }
}

/// The C6 ladder. PURE.
///
/// The first of `coord` / `persisted` that holds an answer decides: a decision
/// is its level, and "no opinion" hands over to the profile, then the product
/// default. With neither, a scope that names a tenant (`tenant_known`) is
/// UNKNOWN and refuses; a scope with no tenant at all (an unpaired device)
/// has no tenant policy to wait for, and the profile, then the product
/// default, answer.
pub(crate) fn resolve(
    coord: Option<Answer>,
    persisted: Option<Answer>,
    profile: Option<Level>,
    tenant_known: bool,
) -> EgressVerdict {
    let verdict = |level: Level, source| EgressVerdict {
        allowed: level == Level::On,
        source,
    };
    let fallthrough = || match profile {
        Some(l) => verdict(l, LevelSource::Profile),
        None => verdict(Level::On, LevelSource::ProductDefault),
    };
    match (coord, persisted) {
        (Some(Answer::Decided(d)), _) => verdict(d.level, LevelSource::Coord(d.decided_by)),
        (Some(Answer::NoOpinion), _) => fallthrough(),
        (None, Some(Answer::Decided(d))) => verdict(d.level, LevelSource::Persisted(d.decided_by)),
        (None, Some(Answer::NoOpinion)) => fallthrough(),
        (None, None) if tenant_known => verdict(Level::Off, LevelSource::Unknown),
        (None, None) => fallthrough(),
    }
}

/// Interpret a profile's raw `egress.default`. PURE.
///
/// `on` / `off` as written. An unrecognised value is `off`: a deployment that
/// wrote SOMETHING into `egress.default` was restricting egress, and a typo
/// must not reopen every flow. Absent or blank is "no profile opinion".
pub(crate) fn profile_level(raw: Option<&str>) -> Option<Level> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    Some(Level::parse(raw).unwrap_or(Level::Off))
}

/// Rung 3 from what the profile loader read. PURE.
///
/// Only an ABSENT profiles.json is "no opinion". A file that is present but
/// unreadable or unparseable, whose existence cannot even be checked, or that
/// names an active profile it does not define, reads `off`: the machine was
/// configured, and a configuration this runner cannot read is never an
/// authorisation to send.
pub(crate) fn profile_rung(read: &qontinui_runner_lib::profiles::ActiveEgress) -> Option<Level> {
    use qontinui_runner_lib::profiles::ActiveEgress;
    match read {
        ActiveEgress::FileAbsent => None,
        ActiveEgress::Profile(egress) => {
            profile_level(egress.as_ref().and_then(|e| e.default.as_deref()))
        }
        ActiveEgress::Unreadable(_) => Some(Level::Off),
    }
}

/// What one 2xx fleet-policy answer means for an egress flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordAnswer {
    /// Coord decided: an explicit tenant row, or a self-hosted deployment's
    /// default. Rung 1, and persisted as rung 2.
    Authoritative(Decision),
    /// No row and coord's PRODUCT default: coord has no opinion for this
    /// tenant. Replaces any decision for the scope (a deleted tenant row must
    /// not live on in the store); the profile answers.
    NoOpinion,
    /// A no-row answer from a coord that does not know the egress family (no
    /// `default_source`), or names a default this runner cannot interpret.
    /// Coord answered, so the scope is no longer UNKNOWN, but it says nothing
    /// that could override a decision already held.
    NotAnEgressAnswer,
}

/// Classify a 2xx `GET /coord/fleet-policy?domain=egress_*` body. PURE.
///
/// Within an authoritative answer, an unreadable level is `off`: coord said
/// something about this flow, and a level we cannot identify is never an
/// authorisation to send.
///
/// The `default_source` values (`deployment_profile`, `product`) are those of
/// the coord change adding the egress family (qontinui-coord branch
/// `agent/eb2155ed4152-01a123e8b4fe/p9c-6-3a-egress-domains-and-ingest`, Phase 6
/// of this plan); until that lands on coord's main they are a runner-side
/// reading of that branch, and any other value is not interpreted.
pub(crate) fn classify_coord_answer(
    effective_level: Option<&str>,
    resolved_scope: Option<&str>,
    default_source: Option<&str>,
) -> CoordAnswer {
    let level = effective_level.and_then(Level::parse).unwrap_or(Level::Off);
    let scope = resolved_scope.map(str::trim).filter(|s| !s.is_empty());
    if scope.is_some_and(|s| s != RESOLVED_SCOPE_NONE) {
        return CoordAnswer::Authoritative(Decision {
            level,
            decided_by: CoordOrigin::TenantRow,
        });
    }
    match default_source.map(str::trim).filter(|s| !s.is_empty()) {
        Some(DEFAULT_SOURCE_DEPLOYMENT_PROFILE) => CoordAnswer::Authoritative(Decision {
            level,
            decided_by: CoordOrigin::DeploymentProfile,
        }),
        Some(DEFAULT_SOURCE_PRODUCT) => CoordAnswer::NoOpinion,
        _ => CoordAnswer::NotAnEgressAnswer,
    }
}

/// Which coord deployment and which tenant an answer is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ScopeKey {
    /// The coord HTTP base, without a trailing slash.
    pub(crate) coord_base: String,
    /// The tenant, when this device could name one.
    pub(crate) tenant_id: Option<Uuid>,
}

impl ScopeKey {
    pub(crate) fn new(coord_base: &str, tenant_id: Option<Uuid>) -> Self {
        Self {
            coord_base: coord_base.trim().trim_end_matches('/').to_string(),
            tenant_id,
        }
    }
}

type Answers = [Option<Answer>; 6];
type ScopedAnswers = HashMap<ScopeKey, Answers>;

/// The persisted-answer store's on-disk shape.
#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    schema: u32,
    written_at: String,
    scopes: Vec<StoreScope>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreScope {
    #[serde(flatten)]
    key: ScopeKey,
    /// domain → `{level, decided_by}` or `{no_opinion: true}`. Unknown domains
    /// and unreadable entries are dropped.
    levels: std::collections::BTreeMap<String, Value>,
}

/// One stored answer, as JSON.
fn encode_answer(answer: Answer) -> Value {
    match answer {
        Answer::Decided(d) => serde_json::to_value(d).unwrap_or(Value::Null),
        Answer::NoOpinion => json!({ "no_opinion": true }),
    }
}

/// One stored answer, from JSON; `None` when unreadable.
fn decode_answer(value: &Value) -> Option<Answer> {
    if value.get("no_opinion").and_then(Value::as_bool) == Some(true) {
        return Some(Answer::NoOpinion);
    }
    serde_json::from_value::<Decision>(value.clone())
        .ok()
        .map(Answer::Decided)
}

/// A schema-1 store: `{domain: "on"|"off"}` with no scope. Its `off` values
/// are carried into `default_scope` as legacy decisions (fail-closed); its `on`
/// values are dropped, since nothing said which coord or tenant chose them.
fn decode_legacy_store(raw: &Value, default_scope: &ScopeKey) -> ScopedAnswers {
    let mut answers: Answers = [None; 6];
    if let Some(levels) = raw.get("levels").and_then(Value::as_object) {
        for (domain, level) in levels {
            let off = level.as_str().and_then(Level::parse) == Some(Level::Off);
            if let (Some(flow), true) = (Flow::from_domain(domain), off) {
                answers[flow.index()] = Some(Answer::Decided(Decision {
                    level: Level::Off,
                    decided_by: CoordOrigin::LegacyStore,
                }));
            }
        }
    }
    let mut out = ScopedAnswers::new();
    if answers.iter().any(Option::is_some) {
        out.insert(default_scope.clone(), answers);
    }
    out
}

/// Decode a store file. PURE. A corrupt file, a foreign schema, unknown
/// domains and unreadable entries all read as absent — never a panic, never a
/// guess. A schema-1 file is migrated ([`decode_legacy_store`]).
fn decode_store(raw: &str, default_scope: &ScopeKey) -> ScopedAnswers {
    let mut out = ScopedAnswers::new();
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            warn!("egress: persisted levels unreadable — treating as absent: {e}");
            return out;
        }
    };
    match value.get("schema").and_then(Value::as_u64) {
        Some(1) => return decode_legacy_store(&value, default_scope),
        Some(s) if s == u64::from(STORE_SCHEMA) => {}
        other => {
            warn!(
                "egress: persisted levels carry schema {other:?} (expected {STORE_SCHEMA}) — \
                 treating as absent"
            );
            return out;
        }
    }
    let file: StoreFile = match serde_json::from_value(value) {
        Ok(f) => f,
        Err(e) => {
            warn!("egress: persisted levels unreadable — treating as absent: {e}");
            return out;
        }
    };
    for scope in file.scopes {
        let mut answers: Answers = [None; 6];
        for (domain, entry) in &scope.levels {
            if let (Some(flow), Some(answer)) = (Flow::from_domain(domain), decode_answer(entry)) {
                answers[flow.index()] = Some(answer);
            }
        }
        out.insert(
            ScopeKey::new(&scope.key.coord_base, scope.key.tenant_id),
            answers,
        );
    }
    out
}

/// Encode a store file. PURE apart from the timestamp.
fn encode_store(scopes: &ScopedAnswers) -> Vec<u8> {
    let mut entries: Vec<StoreScope> = scopes
        .iter()
        .filter(|(_, a)| a.iter().any(Option::is_some))
        .map(|(key, answers)| StoreScope {
            key: key.clone(),
            levels: Flow::ALL
                .into_iter()
                .filter_map(|f| Some((f.domain().to_string(), encode_answer(answers[f.index()]?))))
                .collect(),
        })
        .collect();
    entries.sort_by(|a, b| {
        (&a.key.coord_base, a.key.tenant_id).cmp(&(&b.key.coord_base, b.key.tenant_id))
    });
    let file = StoreFile {
        schema: STORE_SCHEMA,
        written_at: chrono::Utc::now().to_rfc3339(),
        scopes: entries,
    };
    serde_json::to_vec_pretty(&file).unwrap_or_default()
}

/// Read the store at `path`. READS ONLY: a missing file (or a missing parent
/// directory) is first boot and creates nothing.
fn load_store(path: &Path, default_scope: &ScopeKey) -> ScopedAnswers {
    match std::fs::read_to_string(path) {
        Ok(raw) => decode_store(&raw, default_scope),
        Err(_) => ScopedAnswers::new(),
    }
}

/// Write the store atomically, creating the parent directory explicitly here —
/// the one writer — rather than as a side effect of resolving the path (the
/// `persist_briefings` precedent). Best-effort: a failure costs durability
/// across the next restart, never the in-memory state.
fn persist_store(path: &Path, scopes: &ScopedAnswers) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("egress: store dir create failed (best-effort): {e}");
            return;
        }
    }
    if let Err(e) = crate::fs_atomic::atomic_write(path, &encode_store(scopes)) {
        warn!("egress: persisting levels failed (best-effort): {e}");
    }
}

/// The process's switch state: one instance in production ([`state`]), and
/// private instances in tests.
pub(crate) struct EgressState {
    /// Rung 1 — this process's coord answers, per scope.
    coord: RwLock<ScopedAnswers>,
    /// Rung 2 — what the store held at boot, updated as answers are persisted.
    persisted: RwLock<ScopedAnswers>,
    /// The DEFAULT scope: this coord and the tenant a new session is stamped
    /// with ([`crate::session::resolve_new_session_tenant`]). A call that
    /// names no tenant is evaluated here.
    current: RwLock<ScopeKey>,
    /// A default scope the poller has moved to but coord has not answered for
    /// yet. Until it has, a call that names no tenant gets the STRICTER of the
    /// old and new scope's verdicts.
    pending: RwLock<Option<ScopeKey>>,
    /// Every tenant this device is bound to, as the poller last enumerated it
    /// (seeded at construction with the tenants the store holds answers for
    /// under the current coord, so an unresolved session is judged against
    /// them from the first instant). [`SessionScope::Unresolved`] is judged by
    /// the strictest verdict across these and the default scope.
    bound: RwLock<BoundSet>,
    /// Rung 3 — the profile's `egress.default`, read once.
    profile: Option<Level>,
    /// Where rung 2 lives; `None` when the config dir does not resolve.
    store_path: Option<PathBuf>,
    /// Sends each flow's switch refused in this process.
    refused: [AtomicU64; 6],
    /// Telemetry's boot decision: 0 = not decided yet, 1 = allowed, 2 = refused.
    telemetry_boot: AtomicU8,
}

impl EgressState {
    pub(crate) fn new(
        store_path: Option<PathBuf>,
        profile: Option<Level>,
        current: ScopeKey,
    ) -> Self {
        let persisted = store_path
            .as_deref()
            .map(|p| load_store(p, &current))
            .unwrap_or_default();
        let mut seeded: Vec<Uuid> = persisted
            .keys()
            .filter(|k| k.coord_base == current.coord_base)
            .filter_map(|k| k.tenant_id)
            .collect();
        seeded.sort();
        seeded.dedup();
        Self {
            coord: RwLock::new(ScopedAnswers::new()),
            bound: RwLock::new(BoundSet {
                tenants: seeded,
                unbound_device: false,
            }),
            persisted: RwLock::new(persisted),
            current: RwLock::new(current),
            pending: RwLock::new(None),
            profile,
            store_path,
            refused: Default::default(),
            telemetry_boot: AtomicU8::new(0),
        }
    }

    fn lookup(map: &RwLock<ScopedAnswers>, key: &ScopeKey, flow: Flow) -> Option<Answer> {
        map.read()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .and_then(|a| a[flow.index()])
    }

    /// The default scope.
    pub(crate) fn current_scope(&self) -> ScopeKey {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn pending_scope(&self) -> Option<ScopeKey> {
        self.pending
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Move the default scope (the poller, each tick). The move takes effect
    /// once coord has answered for the new scope; until then it is pending.
    pub(crate) fn set_current_scope(&self, key: ScopeKey) {
        let mut pending = self.pending.write().unwrap_or_else(|p| p.into_inner());
        if key == self.current_scope() {
            *pending = None;
        } else {
            *pending = Some(key);
        }
    }

    fn verdict_in(&self, key: &ScopeKey, flow: Flow) -> EgressVerdict {
        resolve(
            Self::lookup(&self.coord, key, flow),
            Self::lookup(&self.persisted, key, flow),
            self.profile,
            key.tenant_id.is_some(),
        )
    }

    /// The C6 verdict for `flow` in `tenant`'s scope (`None` = the default
    /// scope). Lock-only, safe on any path.
    pub(crate) fn permit_for(&self, flow: Flow, tenant: Option<Uuid>) -> EgressVerdict {
        let current = self.current_scope();
        if let Some(t) = tenant {
            return self.verdict_in(&ScopeKey::new(&current.coord_base, Some(t)), flow);
        }
        let old = self.verdict_in(&current, flow);
        match self.pending_scope() {
            // The stricter of the two: a refusal in either scope refuses.
            Some(next) => {
                let new = self.verdict_in(&next, flow);
                if old.allowed && !new.allowed {
                    new
                } else {
                    old
                }
            }
            None => old,
        }
    }

    /// [`Self::permit_for`] in the default scope.
    pub(crate) fn permit(&self, flow: Flow) -> EgressVerdict {
        self.permit_for(flow, None)
    }

    /// Record one enumeration of the tenants this device is bound to (the
    /// poller, each tick). The tenants are MERGED into the set — never
    /// replacing it, so a partial or failed enumeration cannot shrink it to
    /// nothing. `complete` says the enumeration read every source; only a
    /// complete enumeration that found no tenant at all, with nothing seeded
    /// or merged before, marks the device as unbound (no credentials) — the
    /// one state in which an empty set does not refuse.
    pub(crate) fn record_bound_enumeration(&self, tenants: Vec<Uuid>, complete: bool) {
        let mut bound = self.bound.write().unwrap_or_else(|p| p.into_inner());
        bound.unbound_device = complete && tenants.is_empty() && bound.tenants.is_empty();
        bound.tenants.extend(tenants);
        bound.tenants.sort();
        bound.tenants.dedup();
    }

    pub(crate) fn bound_set(&self) -> BoundSet {
        self.bound.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn bound_tenants(&self) -> Vec<Uuid> {
        self.bound_set().tenants
    }

    /// The verdict for a session-keyed send in `scope`. An unresolved tenant
    /// gets the STRICTEST verdict across the default scope and every bound
    /// tenant: the first refusal refuses.
    pub(crate) fn permit_session(&self, flow: Flow, scope: SessionScope) -> EgressVerdict {
        strictest_session_verdict(
            flow,
            scope,
            |t| self.permit_for(flow, t),
            || self.bound_set(),
        )
    }

    /// Apply one classified coord answer for `flow` in scope `key`.
    ///
    /// - authoritative: rung 1 holds the decision;
    /// - no opinion (the product default): rung 1 holds "no opinion", which
    ///   replaces a decision (a deleted tenant row must not live on) and lets
    ///   the profile answer;
    /// - not an egress answer: coord answered, so a scope holding nothing is
    ///   no longer UNKNOWN ("no opinion"); a scope already holding an answer
    ///   keeps it.
    ///
    /// The store is rewritten when (and only when) the persisted value changes.
    /// A pending default scope becomes current once it has an answer.
    pub(crate) fn apply_coord_answer(&self, key: &ScopeKey, flow: Flow, answer: CoordAnswer) {
        let i = flow.index();
        let held =
            Self::lookup(&self.coord, key, flow).or(Self::lookup(&self.persisted, key, flow));
        let next = match answer {
            CoordAnswer::Authoritative(d) => Answer::Decided(d),
            CoordAnswer::NoOpinion => Answer::NoOpinion,
            CoordAnswer::NotAnEgressAnswer => held.unwrap_or(Answer::NoOpinion),
        };
        self.coord
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(key.clone())
            .or_insert([None; 6])[i] = Some(next);
        self.promote_pending(key);
        let snapshot = {
            let mut persisted = self.persisted.write().unwrap_or_else(|p| p.into_inner());
            if persisted.get(key).and_then(|a| a[i]) == Some(next) {
                return;
            }
            persisted.entry(key.clone()).or_insert([None; 6])[i] = Some(next);
            persisted.clone()
        };
        if let Some(path) = &self.store_path {
            persist_store(path, &snapshot);
        }
    }

    fn promote_pending(&self, answered: &ScopeKey) {
        let mut pending = self.pending.write().unwrap_or_else(|p| p.into_inner());
        if pending.as_ref() == Some(answered) {
            *self.current.write().unwrap_or_else(|p| p.into_inner()) = answered.clone();
            *pending = None;
        }
    }

    /// Count one send the switch refused.
    pub(crate) fn count_refused(&self, flow: Flow) {
        self.refused[flow.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn refused(&self, flow: Flow) -> u64 {
        self.refused[flow.index()].load(Ordering::Relaxed)
    }

    fn telemetry_boot(&self) -> Option<bool> {
        match self.telemetry_boot.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// The `/health` `egress` object over this state: `scope` (the default
    /// coord + tenant, any pending move, and how other tenants are treated)
    /// plus one entry per flow, evaluated in the default scope.
    pub(crate) fn health_json(&self) -> Value {
        let current = self.current_scope();
        let mut out = serde_json::Map::new();
        out.insert(
            "scope".to_string(),
            json!({
                "coord_base": current.coord_base,
                "tenant_id": current.tenant_id,
                "pending_tenant_id": self.pending_scope().map(|p| p.tenant_id),
                "note": SCOPE_NOTE,
            }),
        );
        for flow in Flow::ALL {
            let verdict = self.permit(flow);
            let mut entry = json!({
                "allowed": verdict.allowed,
                "source": verdict.source.as_str(),
                "decided_by": verdict.source.decided_by().map(CoordOrigin::as_str),
                "domain": flow.domain(),
                "applies_at_next_start": flow.applies_at_next_start(),
                "refused": self.refused(flow),
            });
            if flow == Flow::Telemetry {
                // What the boot actually installed — which can differ from
                // `allowed` until the next start. `null` before the boot
                // decision ran (never a guessed `true`).
                entry["in_effect"] = json!(self.telemetry_boot());
            }
            out.insert(flow.key().to_string(), entry);
        }
        Value::Object(out)
    }
}

/// Where this instance's store lives — the NON-creating resolver, because the
/// first reader is a read (see `fleet_policy_poller::briefing_store_path`).
#[cfg(not(test))]
fn production_store_path() -> Option<PathBuf> {
    crate::settings::resolve_config_dir()
        .ok()
        .map(|(dir, _source)| dir.join(STORE_FILE))
}

/// Rung 3, read once.
#[cfg(not(test))]
fn production_profile_level() -> Option<Level> {
    let read = qontinui_runner_lib::profiles::active_egress_profile();
    if let qontinui_runner_lib::profiles::ActiveEgress::Unreadable(why) = &read {
        warn!(
            "egress: profiles.json is present but unusable ({why}) — every flow reads OFF at \
             the profile rung until it is fixed"
        );
    }
    let level = profile_rung(&read);
    if let Some(level) = level {
        info!("egress: machine profile rung = {}", level.as_str());
    }
    level
}

/// The default scope a fresh process starts on: the coord base this runner
/// resolves and the tenant a new session is stamped with — the SAME resolver
/// the session paths use, so a session of the default binding and a call that
/// names no tenant land in one scope.
#[cfg(not(test))]
fn production_scope() -> ScopeKey {
    let (base, _source) = qontinui_runner_lib::profiles::coord_base_with_source();
    ScopeKey::new(&base, crate::session::resolve_new_session_tenant())
}

/// The process-global state. Test builds never touch the real config dir or
/// profile: their global starts empty with no tenant (every flow at the
/// product default), and tests pin levels per thread through
/// [`test_support::pin`].
fn state() -> &'static EgressState {
    static STATE: OnceLock<EgressState> = OnceLock::new();
    STATE.get_or_init(|| {
        #[cfg(not(test))]
        {
            EgressState::new(
                production_store_path(),
                production_profile_level(),
                production_scope(),
            )
        }
        // The test global is an UNBOUND device (no credentials): tests that
        // need a bound set fake one per thread.
        #[cfg(test)]
        {
            let s = EgressState::new(None, None, ScopeKey::new("http://coord.test", None));
            s.record_bound_enumeration(Vec::new(), true);
            s
        }
    })
}

/// The C6 verdict for `flow` in `tenant`'s scope (`None` = the default scope).
/// Lock-only — safe from synchronous spawn paths, keystroke-rate relay
/// handlers and the boot sequence.
pub(crate) fn permit_for(flow: Flow, tenant: Option<Uuid>) -> EgressVerdict {
    #[cfg(test)]
    if let Some(level) = test_support::pinned_for(flow, tenant) {
        return EgressVerdict {
            allowed: level == Level::On,
            source: LevelSource::Coord(CoordOrigin::TenantRow),
        };
    }
    state().permit_for(flow, tenant)
}

/// The app handle the session-tenant lookups read the lifecycle store and
/// the AI-session registrar through. Installed once at boot, after both are
/// managed; before that (and in a process that never installs it) every
/// lookup answers `None`, i.e. the default scope.
static SESSION_TENANT_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Install the handle [`terminal_session_tenant`] / [`task_run_session_tenant`]
/// read through. Idempotent: a second install is ignored.
pub(crate) fn install_session_tenant_lookup(app: tauri::AppHandle) {
    let _ = SESSION_TENANT_APP.set(app);
}

/// The scope of the open session on terminal `terminal_id`: its spawn tenant
/// (the lifecycle store's `tenant_id`), [`SessionScope::DeviceDefault`] for a
/// session spawned without a tenant choice, and [`SessionScope::Unresolved`]
/// when no open session is there, the stamp is not a UUID (logged — it names
/// no scope coord could have answered for), or the lookup is not installed.
pub(crate) fn terminal_session_tenant(terminal_id: &str) -> SessionScope {
    #[cfg(test)]
    if let Some(faked) = test_support::faked_session_tenant(terminal_id) {
        return faked;
    }
    use tauri::Manager;
    let Some(app) = SESSION_TENANT_APP.get() else {
        return SessionScope::Unresolved;
    };
    let Some(store) = app.try_state::<std::sync::Arc<
        crate::session::session_lifecycle_store::SessionLifecycleStore,
    >>() else {
        return SessionScope::Unresolved;
    };
    let Some(record) = store.find_open_by_terminal(terminal_id) else {
        return SessionScope::Unresolved;
    };
    match record.tenant_id {
        None => SessionScope::DeviceDefault,
        Some(stamped) => match Uuid::parse_str(&stamped) {
            Ok(t) => SessionScope::Tenant(t),
            Err(_) => {
                warn!(
                    terminal_id,
                    stamped,
                    "egress: session tenant is not a UUID — judged by the strictest bound tenant"
                );
                SessionScope::Unresolved
            }
        },
    }
}

/// The scope of the session behind `task_run_id` (its R4 key): the tenant the
/// AI-session registrar recorded, else [`SessionScope::Unresolved`] — not
/// registered by this process (e.g. after a restart), no tenant resolved, or
/// the lookup not installed.
pub(crate) fn task_run_session_tenant(task_run_id: &str) -> SessionScope {
    #[cfg(test)]
    if let Some(faked) = test_support::faked_session_tenant(task_run_id) {
        return faked;
    }
    use tauri::Manager;
    let tenant = SESSION_TENANT_APP.get().and_then(|app| {
        app.try_state::<std::sync::Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
            .and_then(|r| r.recorded_tenant(task_run_id))
    });
    SessionScope::from_lookup(tenant)
}

/// The scope that owns the repository / worktree content at `path`: the
/// session (open, or closed but still recorded) whose working directory is
/// the CLOSEST ancestor of `path`, compared component-wise on CANONICAL paths
/// (symlinks and `..` resolved; on Windows case-insensitively and without the
/// `\\?\` prefix). Several sessions at that closest directory must agree —
/// sessions of different tenants sharing one checkout make it
/// [`SessionScope::Unresolved`], as does a path that cannot be canonicalized,
/// a path no recorded session works under, a non-UUID stamp, or the lookup
/// not installed. A session spawned without a tenant choice is
/// [`SessionScope::DeviceDefault`].
///
/// Residual (documented, not closed): a worktree whose session record the
/// store has already pruned is attributed to whichever recorded session works
/// in an enclosing directory (typically the primary checkout's) — if that one
/// is on, the pruned session's content follows it.
pub(crate) fn path_session_tenant(path: &str) -> SessionScope {
    #[cfg(test)]
    if let Some(faked) = test_support::faked_session_tenant(path) {
        return faked;
    }
    use tauri::Manager;
    let Some(app) = SESSION_TENANT_APP.get() else {
        return SessionScope::Unresolved;
    };
    let Some(store) = app.try_state::<std::sync::Arc<
        crate::session::session_lifecycle_store::SessionLifecycleStore,
    >>() else {
        return SessionScope::Unresolved;
    };
    // Every record the store still holds — open AND closed — so a worktree
    // whose session has ended is still attributed to that session's tenant
    // rather than to an open session in an enclosing directory. Records that
    // disagree at the closest directory make the path unresolved.
    let records = store.all_records();
    scope_for_path(
        path,
        records
            .iter()
            .filter_map(|r| Some((r.working_dir.as_deref()?, r.tenant_id.as_deref()))),
    )
}

/// A path in the form ownership is compared in: on Windows without the
/// verbatim `\\?\` (or `\\?\UNC\`) prefix, with `/` as `\`, lower-cased (NTFS
/// is case-insensitive); elsewhere unchanged. PURE.
pub(crate) fn normalize_ownership_key(raw: &str, windows: bool) -> PathBuf {
    if !windows {
        return PathBuf::from(raw);
    }
    let unprefixed = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.to_string()
    };
    PathBuf::from(unprefixed.replace('/', "\\").to_lowercase())
}

/// `path` canonicalized (symlinks and `..` resolved) and normalized for
/// comparison; `None` when it cannot be canonicalized (it does not exist, or
/// is unreadable).
fn ownership_key(path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    Some(normalize_ownership_key(
        &canonical.to_string_lossy(),
        cfg!(windows),
    ))
}

/// The rule behind [`path_session_tenant`] over `(working_dir, tenant stamp)`
/// pairs. PURE.
pub(crate) fn scope_for_path<'a>(
    path: &str,
    sessions: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) -> SessionScope {
    // Both sides CANONICAL: `..`, symlinks and (on Windows) case / the
    // verbatim prefix must not let a path borrow another tenant's directory.
    // A target that cannot be canonicalized is unresolved; a session directory
    // that cannot be (deleted since) owns nothing.
    if !Path::new(path).is_absolute() {
        return SessionScope::Unresolved;
    }
    let Some(target) = ownership_key(Path::new(path)) else {
        return SessionScope::Unresolved;
    };
    let mut best: Option<(usize, Vec<Option<&'a str>>)> = None;
    for (dir, stamp) in sessions {
        if !Path::new(dir).is_absolute() {
            continue;
        }
        let Some(dir) = ownership_key(Path::new(dir)) else {
            continue;
        };
        if !target.starts_with(&dir) {
            continue;
        }
        let depth = dir.components().count();
        match &mut best {
            Some((d, stamps)) if *d == depth => stamps.push(stamp),
            Some((d, _)) if *d > depth => {}
            _ => best = Some((depth, vec![stamp])),
        }
    }
    let Some((_, stamps)) = best else {
        return SessionScope::Unresolved;
    };
    let scopes: Vec<SessionScope> = stamps
        .into_iter()
        .map(|stamp| match stamp {
            None => SessionScope::DeviceDefault,
            Some(s) => Uuid::parse_str(s)
                .map(SessionScope::Tenant)
                .unwrap_or(SessionScope::Unresolved),
        })
        .collect();
    match scopes.first() {
        Some(first) if scopes.iter().all(|s| s == first) => *first,
        _ => SessionScope::Unresolved,
    }
}

/// [`permit_for`] in the default scope.
pub(crate) fn permit(flow: Flow) -> EgressVerdict {
    permit_for(flow, None)
}

/// [`permit_for`], counting a refusal. For call sites that are about to drop
/// a send on the switch's word — the counter is what `/health` reports.
pub(crate) fn permit_or_count_for(flow: Flow, tenant: Option<Uuid>) -> bool {
    let allowed = permit_for(flow, tenant).allowed;
    if !allowed {
        state().count_refused(flow);
    }
    allowed
}

/// [`permit_or_count_for`] in the default scope.
pub(crate) fn permit_or_count(flow: Flow) -> bool {
    permit_or_count_for(flow, None)
}

/// The tenants the strictest rule is taken across.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BoundSet {
    /// Every tenant ever enumerated or seeded for this coord (merged, never
    /// replaced).
    pub(crate) tenants: Vec<Uuid>,
    /// A COMPLETE enumeration found no tenant at all: the device holds no
    /// credentials, so there is no tenant whose policy could be violated.
    pub(crate) unbound_device: bool,
}

/// The rule behind [`permit_session`], over an injected per-scope verdict and
/// bound set (`verdict(None)` is the default scope). PURE. An unresolved send
/// with an EMPTY bound set refuses — unless the device is unbound — because
/// "no tenant known" is not "no tenant to protect".
fn strictest_session_verdict(
    flow: Flow,
    scope: SessionScope,
    verdict: impl Fn(Option<Uuid>) -> EgressVerdict,
    bound: impl FnOnce() -> BoundSet,
) -> EgressVerdict {
    let _ = flow;
    match scope {
        SessionScope::Tenant(t) => verdict(Some(t)),
        SessionScope::DeviceDefault => verdict(None),
        SessionScope::Unresolved => {
            let default = verdict(None);
            if !default.allowed {
                return default;
            }
            let bound = bound();
            if bound.tenants.is_empty() && !bound.unbound_device {
                return EgressVerdict {
                    allowed: false,
                    source: LevelSource::Unknown,
                };
            }
            bound
                .tenants
                .into_iter()
                .map(|t| verdict(Some(t)))
                .find(|v| !v.allowed)
                .unwrap_or(default)
        }
    }
}

/// The bound set the poller last recorded (the strictest rule's set).
fn bound_tenants() -> BoundSet {
    #[cfg(test)]
    if let Some(faked) = test_support::faked_bound_tenants() {
        return BoundSet {
            tenants: faked,
            unbound_device: false,
        };
    }
    state().bound_set()
}

/// The verdict for a session-keyed send of a CONTENT flow in `scope` —
/// [`SessionScope::Unresolved`] is judged by the strictest verdict across the
/// default scope and every bound tenant.
pub(crate) fn permit_session(flow: Flow, scope: SessionScope) -> EgressVerdict {
    strictest_session_verdict(flow, scope, |t| permit_for(flow, t), bound_tenants)
}

/// [`permit_session`], counting a refusal.
pub(crate) fn permit_or_count_session(flow: Flow, scope: SessionScope) -> bool {
    let allowed = permit_session(flow, scope).allowed;
    if !allowed {
        state().count_refused(flow);
    }
    allowed
}

/// Transcript sync's gate for a session in `scope` ([`permit_session`] for the
/// tenant half, AND the user's own toggle).
pub(crate) fn transcript_sync_gate_session(scope: SessionScope) -> TranscriptGate {
    transcript_gate_with(
        permit_session(Flow::TranscriptSync, scope).allowed,
        crate::settings::get_cloud_sync_enabled,
    )
}

/// The process's switch state — the poller's write door (it applies answers,
/// moves the default scope and records the bound tenants through it).
pub(crate) fn global_state() -> &'static EgressState {
    state()
}

/// Transcript sync's full consent, with the reason when it is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptGate {
    Open,
    /// The user's own `cloud_sync_enabled` toggle is off.
    UserConsentOff,
    /// The tenant's `egress_transcript_sync` switch is off (or UNKNOWN).
    TenantSwitchOff,
}

impl TranscriptGate {
    pub(crate) const fn is_open(self) -> bool {
        matches!(self, TranscriptGate::Open)
    }
}

/// The gate from its two halves. PURE apart from the injected read. The
/// tenant switch is read first: it is lock-only, and when it is off the
/// settings read is skipped.
pub(crate) fn transcript_gate_with(
    tenant_allowed: bool,
    user_consent: impl FnOnce() -> bool,
) -> TranscriptGate {
    if !tenant_allowed {
        TranscriptGate::TenantSwitchOff
    } else if !user_consent() {
        TranscriptGate::UserConsentOff
    } else {
        TranscriptGate::Open
    }
}

/// Transcript sync's gate for a session of `tenant` (`None` = the default
/// scope): the tenant's `egress_transcript_sync` AND the user's own
/// `cloud_sync_enabled`. Every egress path of the transcript / tenant-memory
/// family reads this (or [`transcript_sync_permitted`]), never the user's
/// toggle bare — `no_bare_cloud_sync_reads_outside_the_allowed_files`
/// enforces it.
pub(crate) fn transcript_sync_gate_for(tenant: Option<Uuid>) -> TranscriptGate {
    transcript_gate_with(
        permit_for(Flow::TranscriptSync, tenant).allowed,
        crate::settings::get_cloud_sync_enabled,
    )
}

/// [`transcript_sync_gate_for`] in the default scope, as a bool.
pub(crate) fn transcript_sync_permitted() -> bool {
    transcript_sync_gate_for(None).is_open()
}

/// [`transcript_sync_permitted`] over an injected user-consent read, so tests
/// drive both halves without touching the machine's `settings.json`.
pub(crate) fn transcript_sync_permitted_with(user_consent: impl FnOnce() -> bool) -> bool {
    transcript_gate_with(permit(Flow::TranscriptSync).allowed, user_consent).is_open()
}

/// Telemetry's boot decision: the verdict now, recorded so `/health` can say
/// what the running process actually installed (`in_effect`). Called once by
/// the `sentry::init` block and once by `otel::init_otel`; both read the same
/// verdict.
pub(crate) fn telemetry_permitted_at_boot() -> bool {
    let verdict = permit(Flow::Telemetry);
    let st = state();
    let code = if verdict.allowed { 1 } else { 2 };
    if st.telemetry_boot.swap(code, Ordering::AcqRel) != code && !verdict.allowed {
        info!(
            "egress: telemetry is off for this project (source={}) — crash reporting and OTLP \
             export stay uninstalled until a start where it is on",
            verdict.source.as_str()
        );
    }
    verdict.allowed
}

/// The human sentence for a refused flow.
pub(crate) const fn refusal_message(flow: Flow) -> &'static str {
    match flow {
        Flow::TerminalStream => "Terminal streaming is off for this project",
        Flow::TranscriptSync => "Transcript sync is off for this project",
        Flow::CodeMirror => "The code mirror is off for this project",
        Flow::Telemetry => "Telemetry is off for this project",
        Flow::UpdateCheck => "Update checks are off for this project",
        Flow::SkillMirror => "The skill mirror is off for this project",
    }
}

/// The refusal frame a relay handler answers instead of sending `flow`'s
/// bytes, typed as the reply the caller is waiting for (`terminal_response`,
/// `chat_output`). `request_id` / `terminal_id` / `session_id` are echoed so
/// the web side can correlate it; the console renders `message`.
pub(crate) fn egress_refusal_frame(reply_type: &str, flow: Flow, data: &Value) -> Value {
    json!({
        "type": reply_type,
        "error": "egress_off",
        "flow": flow.key(),
        "domain": flow.domain(),
        "message": refusal_message(flow),
        "request_id": data.get("request_id").cloned().unwrap_or(Value::Null),
        "terminal_id": data.get("terminal_id").cloned().unwrap_or(Value::Null),
        "session_id": data.get("session_id").cloned().unwrap_or(Value::Null),
    })
}

/// The refusal frame a relay `terminal_*` handler answers instead of streaming.
pub(crate) fn terminal_refusal_frame(data: &Value) -> Value {
    egress_refusal_frame("terminal_response", Flow::TerminalStream, data)
}

/// `GET /health` `egress`: the default `scope`, and every flow's `{allowed,
/// source, decided_by, domain, applies_at_next_start, refused}` (telemetry
/// adds `in_effect`).
pub(crate) fn health_json() -> Value {
    state().health_json()
}

/// Test pins. Per THREAD, so a test that pins a flow off cannot flip a
/// concurrently running test's flow on another thread — the code under test
/// must run on the pinning thread (a `#[tokio::test]` current-thread runtime,
/// or a plain `#[test]`). A pinned level reads as a coord answer.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{Flow, Level};
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    thread_local! {
        static PINS: RefCell<[Option<Level>; 6]> = const { RefCell::new([None; 6]) };
        static TENANT_PINS: RefCell<Vec<(Flow, Uuid, Level)>> = const { RefCell::new(Vec::new()) };
        static SESSION_TENANTS: RefCell<Vec<(String, super::SessionScope)>> = const { RefCell::new(Vec::new()) };
        static BOUND: RefCell<Option<Vec<Uuid>>> = const { RefCell::new(None) };
    }

    pub(crate) fn pinned(flow: Flow) -> Option<Level> {
        PINS.with(|p| p.borrow()[flow.index()])
    }

    /// The pin for `flow` in `tenant`'s scope: a tenant pin when one is set
    /// for that tenant, else the flow-wide pin.
    pub(crate) fn pinned_for(flow: Flow, tenant: Option<Uuid>) -> Option<Level> {
        if let Some(t) = tenant {
            let hit = TENANT_PINS.with(|p| {
                p.borrow()
                    .iter()
                    .rev()
                    .find(|(f, pt, _)| *f == flow && *pt == t)
                    .map(|(_, _, l)| *l)
            });
            if hit.is_some() {
                return hit;
            }
        }
        pinned(flow)
    }

    /// RAII pin for one flow in ONE tenant's scope on this thread.
    pub(crate) struct TenantPin;

    impl Drop for TenantPin {
        fn drop(&mut self) {
            TENANT_PINS.with(|p| {
                p.borrow_mut().pop();
            });
        }
    }

    pub(crate) fn pin_for(flow: Flow, tenant: Uuid, level: Level) -> TenantPin {
        TENANT_PINS.with(|p| p.borrow_mut().push((flow, tenant, level)));
        TenantPin
    }

    /// `Some(answer)` when a test faked the tenant of session key `key` (a
    /// terminal id or a task-run id) on this thread.
    pub(crate) fn faked_session_tenant(key: &str) -> Option<super::SessionScope> {
        SESSION_TENANTS.with(|m| {
            m.borrow()
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, t)| *t)
        })
    }

    /// RAII fake: session key `key` belongs to `tenant` on this thread.
    pub(crate) struct FakeSessionTenant;

    impl Drop for FakeSessionTenant {
        fn drop(&mut self) {
            SESSION_TENANTS.with(|m| {
                m.borrow_mut().pop();
            });
        }
    }

    pub(crate) fn fake_session_tenant(key: &str, scope: super::SessionScope) -> FakeSessionTenant {
        SESSION_TENANTS.with(|m| m.borrow_mut().push((key.to_string(), scope)));
        FakeSessionTenant
    }

    pub(crate) fn faked_bound_tenants() -> Option<Vec<Uuid>> {
        BOUND.with(|b| b.borrow().clone())
    }

    /// RAII fake of the bound-tenant set on this thread.
    pub(crate) struct FakeBound(Option<Vec<Uuid>>);

    impl Drop for FakeBound {
        fn drop(&mut self) {
            let previous = self.0.take();
            BOUND.with(|b| *b.borrow_mut() = previous);
        }
    }

    pub(crate) fn fake_bound_tenants(tenants: Vec<Uuid>) -> FakeBound {
        FakeBound(BOUND.with(|b| b.borrow_mut().replace(tenants)))
    }

    /// RAII pin for one flow on this thread; restored on drop (including on a
    /// failing assertion's unwind).
    pub(crate) struct EgressPin {
        flow: Flow,
        previous: Option<Level>,
    }

    impl Drop for EgressPin {
        fn drop(&mut self) {
            PINS.with(|p| p.borrow_mut()[self.flow.index()] = self.previous);
        }
    }

    pub(crate) fn pin(flow: Flow, level: Level) -> EgressPin {
        let previous = PINS.with(|p| {
            let mut p = p.borrow_mut();
            let prev = p[flow.index()];
            p[flow.index()] = Some(level);
            prev
        });
        EgressPin { flow, previous }
    }

    /// A loopback listener that counts the TCP connections made to it — the
    /// observable for "the flow made zero / at least one connection attempt".
    /// Each connection gets a `503` and is closed, so an HTTP client fails
    /// fast instead of waiting on a timeout.
    pub(crate) struct ConnCounter {
        addr: SocketAddr,
        count: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl ConnCounter {
        pub(crate) fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");
            let count = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let (c, s) = (count.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if s.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    c.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            });
            Self { addr, count, stop }
        }

        pub(crate) fn port(&self) -> u16 {
            self.addr.port()
        }

        pub(crate) fn http_base(&self) -> String {
            format!("http://127.0.0.1:{}", self.addr.port())
        }

        pub(crate) fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }

        /// Poll until at least `n` connections arrived or `within` elapsed —
        /// for flows whose send completes on a background thread.
        pub(crate) fn wait_for(&self, n: usize, within: Duration) -> usize {
            let deadline = Instant::now() + within;
            while self.count() < n && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            self.count()
        }
    }

    impl Drop for ConnCounter {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Unblock the accept loop so its thread ends.
            let _ = TcpStream::connect(self.addr);
        }
    }
}

/// Register, for every [`Flow`], the pair of tests proving its gate: pinned
/// `off` makes zero connection attempts, pinned `on` makes at least one. The
/// `match` is exhaustive, so a flow without a registration is a compile error,
/// and each entry coerces the named test to `fn()`, so a registration naming a
/// test that does not exist is a compile error too.
#[cfg(test)]
macro_rules! register_flow_tests {
    ($($flow:ident => { off: $off:path, on: $on:path $(,)? }),+ $(,)?) => {
        /// The registered pairs, by name, for the census test.
        pub(crate) const FLOW_TESTS: &[(Flow, &str, &str)] =
            &[$((Flow::$flow, stringify!($off), stringify!($on))),+];

        #[allow(dead_code)]
        fn every_flow_registers_its_tests(flow: Flow) -> (fn(), fn()) {
            match flow {
                $(Flow::$flow => ($off as fn(), $on as fn()),)+
            }
        }
    };
}

#[cfg(test)]
register_flow_tests! {
    TranscriptSync => {
        off: crate::session::coord_sync::egress_tests::transcript_chunk_pinned_off_makes_zero_requests,
        on: crate::session::coord_sync::egress_tests::transcript_chunk_pinned_on_makes_a_request,
    },
    CodeMirror => {
        off: crate::agent_pusher::egress_tests::code_mirror_pinned_off_makes_zero_connections,
        on: crate::agent_pusher::egress_tests::code_mirror_pinned_on_makes_a_connection,
    },
    TerminalStream => {
        off: crate::session::output_pipe::egress_tests::terminal_stream_pinned_off_makes_zero_connections,
        on: crate::session::output_pipe::egress_tests::terminal_stream_pinned_on_makes_a_connection,
    },
    Telemetry => {
        off: crate::otel::egress_tests::telemetry_pinned_off_makes_zero_connections,
        on: crate::otel::egress_tests::telemetry_pinned_on_makes_a_connection,
    },
    UpdateCheck => {
        off: crate::commands::execution::system_ops::egress_tests::update_check_pinned_off_makes_zero_connections,
        on: crate::commands::execution::system_ops::egress_tests::update_check_pinned_on_makes_a_connection,
    },
    SkillMirror => {
        off: crate::canonical_corpus::egress_tests::skill_mirror_pinned_off_makes_zero_connections,
        on: crate::canonical_corpus::egress_tests::skill_mirror_pinned_on_makes_a_connection,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const TENANT_A: Uuid = Uuid::from_u128(0xA);
    const TENANT_B: Uuid = Uuid::from_u128(0xB);

    fn row(level: Level) -> CoordAnswer {
        CoordAnswer::Authoritative(Decision {
            level,
            decided_by: CoordOrigin::TenantRow,
        })
    }

    fn key(tenant: Option<Uuid>) -> ScopeKey {
        ScopeKey::new("http://coord.example/", tenant)
    }

    #[test]
    fn resolution_takes_the_first_rung_with_a_value() {
        use Level::{Off, On};
        let d = |level| {
            Some(Answer::Decided(Decision {
                level,
                decided_by: CoordOrigin::TenantRow,
            }))
        };
        let no = Some(Answer::NoOpinion);
        let row = LevelSource::Coord(CoordOrigin::TenantRow);
        let kept = LevelSource::Persisted(CoordOrigin::TenantRow);
        let cases = [
            ((d(Off), d(On), Some(On)), (false, row)),
            ((d(On), d(Off), Some(Off)), (true, row)),
            ((None, d(Off), Some(On)), (false, kept)),
            ((None, d(On), Some(Off)), (true, kept)),
            ((None, None, Some(Off)), (false, LevelSource::Profile)),
            ((None, None, Some(On)), (true, LevelSource::Profile)),
            ((None, None, None), (true, LevelSource::ProductDefault)),
            // "No opinion" hands over to the profile, even over a stale
            // persisted decision.
            ((no, d(Off), Some(On)), (true, LevelSource::Profile)),
            ((None, no, None), (true, LevelSource::ProductDefault)),
        ];
        for ((c, p, prof), (allowed, source)) in cases {
            assert_eq!(
                resolve(c, p, prof, false),
                EgressVerdict { allowed, source },
                "coord={c:?} persisted={p:?} profile={prof:?}"
            );
        }
        // A scope naming a tenant with no answer at all is UNKNOWN and
        // refuses, whatever the profile says; an answer clears it.
        for prof in [None, Some(On), Some(Off)] {
            assert_eq!(
                resolve(None, None, prof, true),
                EgressVerdict {
                    allowed: false,
                    source: LevelSource::Unknown
                }
            );
        }
        assert_eq!(
            resolve(no, None, None, true).source,
            LevelSource::ProductDefault
        );
    }

    #[test]
    fn only_a_tenant_row_or_a_deployment_default_is_authoritative() {
        // A coord that predates the egress family: its generic `off`.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), None),
            CoordAnswer::NotAnEgressAnswer
        );
        assert_eq!(
            classify_coord_answer(Some("off"), None, None),
            CoordAnswer::NotAnEgressAnswer
        );
        // H1: coord's PRODUCT default is no opinion — never a decision.
        assert_eq!(
            classify_coord_answer(Some("on"), Some("none"), Some("product")),
            CoordAnswer::NoOpinion
        );
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), Some("product")),
            CoordAnswer::NoOpinion
        );
        // A default source this runner cannot interpret changes nothing.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), Some("something_new")),
            CoordAnswer::NotAnEgressAnswer
        );
        // A self-hosted deployment's default is a decision, labelled as such.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("none"), Some("deployment_profile")),
            CoordAnswer::Authoritative(Decision {
                level: Level::Off,
                decided_by: CoordOrigin::DeploymentProfile
            })
        );
        // An explicit row is the tenant's, with or without the new field.
        assert_eq!(
            classify_coord_answer(Some("off"), Some("tenant"), None),
            row(Level::Off)
        );
        assert_eq!(
            classify_coord_answer(Some(" ON "), Some("tenant"), None),
            row(Level::On)
        );
        // An unreadable level in an authoritative answer fails closed.
        assert_eq!(
            classify_coord_answer(Some("record"), Some("tenant"), None),
            row(Level::Off)
        );
    }

    #[test]
    fn profile_default_reads_on_off_and_fails_closed_on_anything_else() {
        assert_eq!(profile_level(None), None);
        assert_eq!(profile_level(Some("  ")), None);
        assert_eq!(profile_level(Some("on")), Some(Level::On));
        assert_eq!(profile_level(Some("OFF")), Some(Level::Off));
        assert_eq!(profile_level(Some("disabled")), Some(Level::Off));
    }

    /// M2: only an ABSENT profiles.json is "no opinion"; a present file this
    /// runner cannot use — unparseable, or naming an active profile it does
    /// not define — reads OFF.
    #[test]
    fn an_unusable_profiles_file_reads_off_and_only_an_absent_one_is_no_opinion() {
        use qontinui_runner_lib::profiles::active_egress_profile_at;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        assert_eq!(profile_rung(&active_egress_profile_at(&path, None)), None);

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(
            profile_rung(&active_egress_profile_at(&path, None)),
            Some(Level::Off)
        );

        std::fs::write(&path, br#"{"active":"prod","profiles":{"dev":{}}}"#).unwrap();
        assert_eq!(
            profile_rung(&active_egress_profile_at(&path, None)),
            Some(Level::Off),
            "an active profile the file does not define"
        );

        std::fs::write(&path, br#"{"active":"dev","profiles":{"dev":{}}}"#).unwrap();
        assert_eq!(profile_rung(&active_egress_profile_at(&path, None)), None);
        std::fs::write(
            &path,
            br#"{"active":"dev","profiles":{"dev":{"egress":{"default":"off"}},"x":{}}}"#,
        )
        .unwrap();
        assert_eq!(
            profile_rung(&active_egress_profile_at(&path, None)),
            Some(Level::Off)
        );
        // Re-review 7: a profiles.json whose existence cannot even be
        // checked is unusable, not absent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = dir.path().join("locked");
            std::fs::create_dir(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let hidden = locked.join("profiles.json");
            let readable = std::fs::metadata(&hidden).is_ok();
            let read = active_egress_profile_at(&hidden, None);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            if !readable {
                assert_eq!(profile_rung(&read), Some(Level::Off), "{read:?}");
            }
        }
        // The env override picks the profile, as at runtime.
        assert_eq!(
            profile_rung(&active_egress_profile_at(&path, Some("x".into()))),
            None
        );
    }

    #[test]
    fn a_coord_answer_persists_and_the_next_process_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(STORE_FILE);

        let first = EgressState::new(Some(path.clone()), None, key(Some(TENANT_A)));
        assert_eq!(
            first.permit(Flow::CodeMirror).source,
            LevelSource::Unknown,
            "a tenant scope with no answer yet"
        );
        first.apply_coord_answer(&key(Some(TENANT_A)), Flow::CodeMirror, row(Level::Off));
        assert_eq!(
            first.permit(Flow::CodeMirror),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Coord(CoordOrigin::TenantRow)
            }
        );
        assert!(path.exists(), "the answer must be written to the store");

        // A new process: no coord answer yet, so rung 2 answers.
        let second = EgressState::new(Some(path.clone()), Some(Level::On), key(Some(TENANT_A)));
        assert_eq!(
            second.permit(Flow::CodeMirror),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Persisted(CoordOrigin::TenantRow)
            }
        );
        // A flow coord never answered for this tenant is UNKNOWN.
        assert_eq!(
            second.permit(Flow::Telemetry),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Unknown
            }
        );
    }

    /// H1: a product-default answer clears a stale persisted decision (a
    /// deleted tenant row) and never outranks the profile.
    #[test]
    fn a_product_default_clears_the_scope_and_the_profile_answers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let k = key(Some(TENANT_A));
        let st = EgressState::new(Some(path.clone()), Some(Level::Off), k.clone());
        st.apply_coord_answer(&k, Flow::SkillMirror, row(Level::On));
        assert!(st.permit(Flow::SkillMirror).allowed, "the tenant row wins");
        st.apply_coord_answer(&k, Flow::SkillMirror, CoordAnswer::NoOpinion);
        assert_eq!(
            st.permit(Flow::SkillMirror),
            EgressVerdict {
                allowed: false,
                source: LevelSource::Profile
            }
        );
        let restarted = EgressState::new(Some(path), Some(Level::Off), k);
        assert_eq!(
            restarted.permit(Flow::SkillMirror).source,
            LevelSource::Profile
        );
        // A pre-egress coord changes nothing, in either direction.
        restarted.apply_coord_answer(
            &key(Some(TENANT_A)),
            Flow::SkillMirror,
            CoordAnswer::NotAnEgressAnswer,
        );
        assert_eq!(
            restarted.permit(Flow::SkillMirror).source,
            LevelSource::Profile
        );
    }

    /// M3: answers are keyed by (coord base, tenant). Another tenant's — or
    /// another coord's — switch never applies, and a session naming its
    /// tenant is evaluated in that tenant's scope.
    #[test]
    fn answers_are_scoped_by_coord_and_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let st = EgressState::new(Some(path.clone()), None, key(Some(TENANT_A)));
        st.apply_coord_answer(&key(Some(TENANT_A)), Flow::TerminalStream, row(Level::Off));
        assert!(!st.permit(Flow::TerminalStream).allowed);
        assert!(!st.permit_for(Flow::TerminalStream, Some(TENANT_A)).allowed);
        assert_eq!(
            st.permit_for(Flow::TerminalStream, Some(TENANT_B)).source,
            LevelSource::Unknown,
            "tenant A's answer is not tenant B's"
        );
        st.apply_coord_answer(&key(Some(TENANT_B)), Flow::TerminalStream, row(Level::On));
        assert!(st.permit_for(Flow::TerminalStream, Some(TENANT_B)).allowed);
        // Re-paired into tenant B: the default scope moves with it.
        st.set_current_scope(key(Some(TENANT_B)));
        st.apply_coord_answer(&key(Some(TENANT_B)), Flow::TerminalStream, row(Level::On));
        assert!(st.permit(Flow::TerminalStream).allowed);
        // Re-pointed at another coord: none of the first coord's answers.
        let other = EgressState::new(
            Some(path.clone()),
            None,
            ScopeKey::new("https://other-coord.example", Some(TENANT_A)),
        );
        assert_eq!(
            other.permit(Flow::TerminalStream).source,
            LevelSource::Unknown
        );
        // The same coord + tenant after a restart: restored.
        let back = EgressState::new(Some(path), None, key(Some(TENANT_A)));
        assert_eq!(
            back.permit(Flow::TerminalStream).source,
            LevelSource::Persisted(CoordOrigin::TenantRow)
        );
        // A trailing slash is the same coord.
        assert_eq!(key(None), ScopeKey::new("http://coord.example", None));
    }

    /// Re-review 1: the poller keyed its answers by the machine.json pin
    /// (`None` on an unpinned device) while sessions are stamped with the
    /// default binding (`Some(T)`); a session must still see the decision.
    #[test]
    fn a_session_of_the_default_binding_sees_the_polled_decision() {
        let t = Uuid::from_u128(0x77);
        let st = EgressState::new(None, None, key(None));
        st.apply_coord_answer(&key(None), Flow::TerminalStream, row(Level::Off));
        assert!(
            !st.permit_for(Flow::TerminalStream, Some(t)).allowed,
            "the default binding's session must not fall to the product default"
        );
    }

    /// Re-review 2: a TENANT with neither a polled nor a persisted decision is
    /// UNKNOWN and fails closed — never the product default.
    #[test]
    fn a_known_tenant_without_any_decision_fails_closed() {
        let st = EgressState::new(None, None, key(Some(TENANT_A)));
        let v = st.permit(Flow::CodeMirror);
        assert!(!v.allowed, "UNKNOWN must refuse, got {v:?}");
        assert_eq!(v.source.as_str(), "unknown");
    }

    /// Re-review 5: moving the polled scope keeps the stricter of the old and
    /// new scope until the new one has an answer.
    #[test]
    fn a_scope_move_keeps_the_stricter_answer_until_the_new_scope_answers() {
        let st = EgressState::new(None, None, key(Some(TENANT_A)));
        st.apply_coord_answer(&key(Some(TENANT_A)), Flow::SkillMirror, row(Level::Off));
        st.apply_coord_answer(&key(Some(TENANT_B)), Flow::SkillMirror, row(Level::On));
        st.set_current_scope(key(Some(TENANT_B)));
        assert!(
            !st.permit(Flow::SkillMirror).allowed,
            "old scope's off still holds"
        );
        st.apply_coord_answer(&key(Some(TENANT_B)), Flow::SkillMirror, row(Level::On));
        assert!(
            st.permit(Flow::SkillMirror).allowed,
            "the new scope answered: it governs"
        );
    }

    /// Re-review 6: a schema-1 store's `off` values migrate into the default
    /// scope (fail-closed); its `on` values are dropped.
    #[test]
    fn a_schema_one_store_migrates_its_offs_into_the_default_scope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        std::fs::write(
            &path,
            r#"{"schema":1,"written_at":"x","levels":{"egress_code_mirror":"off","egress_telemetry":"on"}}"#,
        )
        .unwrap();
        let st = EgressState::new(Some(path), Some(Level::On), key(None));
        assert!(
            !st.permit(Flow::CodeMirror).allowed,
            "a legacy off survives"
        );
        assert_eq!(
            st.permit(Flow::Telemetry).source,
            LevelSource::Profile,
            "a legacy on is dropped"
        );
    }

    #[test]
    fn a_steady_state_answer_does_not_rewrite_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let k = key(None);
        let st = EgressState::new(Some(path.clone()), None, k.clone());
        st.apply_coord_answer(&k, Flow::Telemetry, row(Level::Off));
        std::fs::write(&path, b"sentinel").unwrap();
        st.apply_coord_answer(&k, Flow::Telemetry, row(Level::Off));
        assert_eq!(std::fs::read(&path).unwrap(), b"sentinel");
        st.apply_coord_answer(&k, Flow::Telemetry, row(Level::On));
        assert_ne!(std::fs::read(&path).unwrap(), b"sentinel");
    }

    #[test]
    fn a_corrupt_or_foreign_store_reads_as_absent() {
        let k = key(None);
        assert!(decode_store("{not json", &k).is_empty());
        assert!(decode_store(r#"{"schema":9,"written_at":"x","scopes":[]}"#, &k).is_empty());
        assert!(decode_store(r#"{"schema":1,"written_at":"x","levels":{}}"#, &k).is_empty());
        let mixed = decode_store(
            r#"{"schema":2,"written_at":"x","scopes":[{"coord_base":"http://c/","tenant_id":null,
                "levels":{"egress_telemetry":{"level":"off","decided_by":"tenant_row"},
                "egress_unknown":{"level":"off","decided_by":"tenant_row"},
                "egress_code_mirror":{"level":"maybe","decided_by":"tenant_row"},
                "egress_skill_mirror":{"no_opinion":true}}}]}"#,
            &k,
        );
        let d = mixed[&ScopeKey::new("http://c", None)];
        assert!(matches!(
            d[Flow::Telemetry.index()],
            Some(Answer::Decided(Decision {
                level: Level::Off,
                ..
            }))
        ));
        assert_eq!(d[Flow::CodeMirror.index()], None);
        assert_eq!(d[Flow::SkillMirror.index()], Some(Answer::NoOpinion));
        // A missing file creates nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join(STORE_FILE);
        assert!(load_store(&path, &k).is_empty());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let mut scopes = ScopedAnswers::new();
        let mut a: Answers = [None; 6];
        a[Flow::SkillMirror.index()] = Some(Answer::Decided(Decision {
            level: Level::Off,
            decided_by: CoordOrigin::DeploymentProfile,
        }));
        a[Flow::Telemetry.index()] = Some(Answer::NoOpinion);
        let mut b: Answers = [None; 6];
        b[Flow::UpdateCheck.index()] = Some(Answer::Decided(Decision {
            level: Level::On,
            decided_by: CoordOrigin::TenantRow,
        }));
        scopes.insert(key(Some(TENANT_A)), a);
        scopes.insert(key(None), b);
        let raw = String::from_utf8(encode_store(&scopes)).unwrap();
        assert_eq!(decode_store(&raw, &key(None)), scopes);
    }

    #[test]
    fn health_json_reports_the_scope_and_all_six_flows_with_their_source() {
        let st = EgressState::new(None, Some(Level::Off), key(Some(TENANT_A)));
        st.apply_coord_answer(&key(Some(TENANT_A)), Flow::CodeMirror, row(Level::Off));
        st.apply_coord_answer(
            &key(Some(TENANT_A)),
            Flow::UpdateCheck,
            CoordAnswer::Authoritative(Decision {
                level: Level::Off,
                decided_by: CoordOrigin::DeploymentProfile,
            }),
        );
        st.count_refused(Flow::CodeMirror);
        let v = st.health_json();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 7, "the scope plus six flows");
        assert_eq!(v["scope"]["coord_base"], "http://coord.example");
        assert_eq!(v["scope"]["tenant_id"], json!(TENANT_A));
        assert!(v["scope"]["note"].as_str().unwrap().contains("UNKNOWN"));
        for flow in Flow::ALL {
            let e = &obj[flow.key()];
            assert_eq!(e["domain"], flow.domain());
            assert!(e["allowed"].is_boolean());
        }
        assert_eq!(v["code_mirror"]["source"], "coord");
        assert_eq!(
            v["code_mirror"]["decided_by"], "tenant_row",
            "L1: which coord default/row"
        );
        assert_eq!(v["update_check"]["decided_by"], "deployment_profile");
        assert_eq!(v["code_mirror"]["allowed"], false);
        assert_eq!(v["code_mirror"]["refused"], 1);
        assert_eq!(
            v["telemetry"]["source"], "unknown",
            "tenant A has no telemetry answer"
        );
        assert_eq!(v["telemetry"]["allowed"], false);
        assert!(v["telemetry"]["decided_by"].is_null());
        assert_eq!(v["telemetry"]["applies_at_next_start"], true);
        assert!(
            v["telemetry"]["in_effect"].is_null(),
            "no boot decision yet"
        );
        assert_eq!(v["skill_mirror"]["applies_at_next_start"], false);
    }

    #[test]
    fn the_transcript_gate_names_which_half_closed_it() {
        assert_eq!(transcript_gate_with(true, || true), TranscriptGate::Open);
        assert_eq!(
            transcript_gate_with(true, || false),
            TranscriptGate::UserConsentOff
        );
        assert_eq!(
            transcript_gate_with(false, || true),
            TranscriptGate::TenantSwitchOff
        );
    }

    #[test]
    fn the_relay_refusal_frame_is_typed_as_the_awaited_reply() {
        let data = json!({"request_id": "r", "session_id": "s"});
        let f = egress_refusal_frame("chat_output", Flow::TerminalStream, &data);
        assert_eq!(f["type"], "chat_output");
        assert_eq!(f["error"], "egress_off");
        assert_eq!(f["session_id"], "s");
        assert_eq!(terminal_refusal_frame(&data)["type"], "terminal_response");
    }

    #[test]
    fn the_test_pin_is_per_thread_and_restores() {
        assert_eq!(
            permit(Flow::SkillMirror).source,
            LevelSource::ProductDefault
        );
        {
            let _pin = test_support::pin(Flow::SkillMirror, Level::Off);
            assert!(!permit(Flow::SkillMirror).allowed);
            let other = std::thread::spawn(|| permit(Flow::SkillMirror).allowed)
                .join()
                .unwrap();
            assert!(other, "a pin must not leak to another thread");
        }
        assert!(permit(Flow::SkillMirror).allowed);
    }

    #[test]
    fn transcript_sync_needs_both_the_user_and_the_tenant() {
        let _on = test_support::pin(Flow::TranscriptSync, Level::On);
        assert!(transcript_sync_permitted_with(|| true));
        assert!(!transcript_sync_permitted_with(|| false));
        drop(_on);
        let _off = test_support::pin(Flow::TranscriptSync, Level::Off);
        let mut asked = false;
        assert!(!transcript_sync_permitted_with(|| {
            asked = true;
            true
        }));
        assert!(
            !asked,
            "with the tenant switch off the settings read is skipped"
        );
    }

    /// Census: six flows, six distinct domains in vendored order, every flow
    /// registered with a distinct pair of tests.
    #[test]
    fn census_six_flows_six_domains_every_flow_tested() {
        assert_eq!(Flow::ALL.len(), 6);
        let domains: Vec<&str> = Flow::ALL.iter().map(|f| f.domain()).collect();
        assert_eq!(domains, EGRESS_DOMAINS.to_vec());
        assert_eq!(domains.iter().collect::<HashSet<_>>().len(), 6);
        for d in EGRESS_DOMAINS {
            assert!(d.starts_with("egress_"), "{d}");
            assert_eq!(Flow::from_domain(d).map(|f| f.domain()), Some(d));
            assert_eq!(
                d.strip_prefix("egress_"),
                Some(Flow::from_domain(d).unwrap().key())
            );
        }
        assert_eq!(FLOW_TESTS.len(), 6);
        let flows: HashSet<Flow> = FLOW_TESTS.iter().map(|(f, _, _)| *f).collect();
        assert_eq!(flows.len(), 6, "each flow registered exactly once");
        let names: HashSet<&str> = FLOW_TESTS
            .iter()
            .flat_map(|(_, off, on)| [*off, *on])
            .collect();
        assert_eq!(names.len(), 12, "twelve distinct tests");
        for flow in Flow::ALL {
            let (off, on) = every_flow_registers_its_tests(flow);
            assert_ne!(off as usize, on as usize);
        }
    }

    /// Where coord's `fleet_policy.rs` sits relative to this crate when the
    /// sibling checkout is present (an allocated worktree's sibling, or the
    /// workspace's primary checkout).
    fn sibling_coord_fleet_policy() -> Option<PathBuf> {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rel = Path::new("qontinui-coord/crates/coord/src/fleet_policy.rs");
        // <workspace>/qontinui-runner/src-tauri or
        // <workspace>/agent-worktrees/<agent>/qontinui-runner/src-tauri.
        manifest
            .ancestors()
            .skip(2)
            .take(3)
            .map(|dir| dir.join(rel))
            .find(|p| p.is_file())
    }

    /// Extract the members of `EGRESS_DOMAINS` from coord's source: string
    /// literals inside the declaration's brackets, or identifiers resolved
    /// through `const IDENT: &str = "…";` in the same file. `None` when the
    /// file does not declare it yet.
    fn parse_coord_egress_domains(src: &str) -> Option<Vec<String>> {
        let decl = src
            .match_indices("EGRESS_DOMAINS")
            .filter_map(|(i, _)| src.get(i..))
            // The declaration itself: `EGRESS_DOMAINS: <type> =` on one line
            // (a doc-comment mention has no `=` before its line ends).
            .find(|rest| {
                let line = rest.lines().next().unwrap_or("");
                line.contains('=') && line.split('=').next().unwrap_or("").contains(':')
            })?;
        let (_, after_eq) = decl.split_once('=')?;
        let (_, from_open) = after_eq.split_once('[')?;
        let (body, _) = from_open.split_once(']')?;
        let mut members = Vec::new();
        for item in body.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(lit) = item.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                members.push(lit.to_string());
                continue;
            }
            let ident = item.rsplit("::").next().unwrap_or(item).trim();
            let needle = format!("const {ident}: &str = \"");
            let (_, value) = src.split_once(needle.as_str())?;
            let (value, _) = value.split_once('"')?;
            members.push(value.to_string());
        }
        Some(members)
    }

    #[test]
    fn the_coord_domain_parser_reads_literals_and_constants() {
        let lits = r#"pub const EGRESS_DOMAINS: [&str; 2] = ["egress_a", "egress_b"];"#;
        assert_eq!(
            parse_coord_egress_domains(lits),
            Some(vec!["egress_a".to_string(), "egress_b".to_string()])
        );
        let consts = r#"
            pub const EGRESS_A_DOMAIN: &str = "egress_a";
            /// mentions EGRESS_DOMAINS in a doc first
            pub const EGRESS_DOMAINS: [&str; 1] = [
                EGRESS_A_DOMAIN,
            ];
        "#;
        assert_eq!(
            parse_coord_egress_domains(consts),
            Some(vec!["egress_a".to_string()])
        );
        assert_eq!(parse_coord_egress_domains("pub const OTHER: u8 = 1;"), None);
    }

    /// Drift: coord's `EGRESS_DOMAINS` and the vendored copy agree on members.
    ///
    /// Runs only when the build found a sibling coord checkout declaring
    /// `EGRESS_DOMAINS` (`build.rs` sets `cfg(coord_egress_sibling)`); otherwise
    /// it is IGNORED with an UNKNOWN reason, so a run without the sibling
    /// reports "ignored", never "ok". FAILS when coord declares it with
    /// different members.
    #[test]
    #[cfg_attr(
        not(coord_egress_sibling),
        ignore = "UNKNOWN: no sibling qontinui-coord checkout declares EGRESS_DOMAINS, so the \
                  egress drift check cannot run"
    )]
    fn vendored_domains_match_coord() {
        let Some(path) = sibling_coord_fleet_policy() else {
            if cfg!(coord_egress_sibling) {
                panic!("the build saw a sibling coord declaring EGRESS_DOMAINS, but it is gone");
            }
            println!(
                "UNKNOWN: egress drift check not run — no sibling qontinui-coord checkout \
                 near {}",
                env!("CARGO_MANIFEST_DIR")
            );
            return;
        };
        let src = std::fs::read_to_string(&path).expect("read coord fleet_policy.rs");
        let Some(coord) = parse_coord_egress_domains(&src) else {
            // The build saw a declaration; a parse that finds none is a failure
            // of this check, not a pass.
            if cfg!(coord_egress_sibling) {
                panic!(
                    "{} declares EGRESS_DOMAINS but its members could not be read",
                    path.display()
                );
            }
            println!(
                "UNKNOWN: egress drift check not run — {} does not declare EGRESS_DOMAINS yet",
                path.display()
            );
            return;
        };
        let mut coord_sorted = coord.clone();
        coord_sorted.sort();
        let mut ours: Vec<String> = EGRESS_DOMAINS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(
            coord_sorted,
            ours,
            "coord's EGRESS_DOMAINS ({}) and the runner's vendored copy disagree",
            path.display()
        );
        println!("egress drift check PASSED against {}", path.display());
    }

    /// `src` with every comment blanked (string literals kept), and a copy with
    /// string literals blanked too. Newlines are kept, so byte offsets map to
    /// the same lines in all three. Handles `//`, `/* */`, `"…"` with escapes,
    /// raw strings `r#"…"#`, and char literals (so `'"'` is not a string).
    fn strip_rust(src: &str) -> (String, String) {
        let b = src.as_bytes();
        let mut code = b.to_vec();
        let mut bare = b.to_vec();
        let blank = |v: &mut Vec<u8>, from: usize, to: usize| {
            for c in v.iter_mut().take(to).skip(from) {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
        };
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    let end = b[i..]
                        .iter()
                        .position(|&c| c == b'\n')
                        .map_or(b.len(), |p| i + p);
                    blank(&mut code, i, end);
                    blank(&mut bare, i, end);
                    i = end;
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let end = src
                        .get(i + 2..)
                        .and_then(|r| r.find("*/"))
                        .map_or(b.len(), |p| i + 2 + p + 2);
                    blank(&mut code, i, end);
                    blank(&mut bare, i, end);
                    i = end;
                }
                b'r' if matches!(b.get(i + 1), Some(b'"') | Some(b'#'))
                    && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
                {
                    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                    if b.get(i + 1 + hashes) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    let close = format!("\"{}", "#".repeat(hashes));
                    let body = i + 2 + hashes;
                    let end = src
                        .get(body..)
                        .and_then(|r| r.find(&close))
                        .map_or(b.len(), |p| body + p + close.len());
                    blank(&mut bare, i, end);
                    i = end;
                }
                b'"' => {
                    let mut j = i + 1;
                    while j < b.len() && b[j] != b'"' {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    let end = (j + 1).min(b.len());
                    blank(&mut bare, i, end);
                    i = end;
                }
                b'\'' => {
                    // A char literal (`'x'`, `'\n'`, `'"'`) — not a lifetime.
                    let len = if b.get(i + 1) == Some(&b'\\') {
                        b[i + 2..].iter().position(|&c| c == b'\'').map(|p| p + 3)
                    } else if b.get(i + 2) == Some(&b'\'') {
                        Some(3)
                    } else {
                        None
                    };
                    i += len.unwrap_or(1);
                }
                _ => i += 1,
            }
        }
        (
            String::from_utf8_lossy(&code).into_owned(),
            String::from_utf8_lossy(&bare).into_owned(),
        )
    }

    /// 1-based lines of `src` that read the user's transcript-sync consent
    /// bare: the getter; the `cloud_sync_enabled` identifier anywhere in code
    /// (a field read, a destructuring, a struct carrying the raw toggle); a
    /// JSON index `x["cloud_sync_enabled"]`; and the settings command
    /// `get_cloud_sync_settings(…)` outside the module that defines it
    /// (`owns_settings_getter`). Comments never count, nor does the word inside
    /// an ordinary string (a log line, a schema).
    fn consent_read_lines(src: &str, owns_settings_getter: bool) -> Vec<usize> {
        let (code, bare) = strip_rust(src);
        let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let mut offsets = Vec::new();
        let word = "cloud_sync_enabled";
        for (i, _) in bare.match_indices(word) {
            let before = i.checked_sub(1).map(|j| bare.as_bytes()[j]);
            let after = bare.as_bytes().get(i + word.len()).copied();
            if !before.is_some_and(ident) && !after.is_some_and(ident) {
                offsets.push(i);
            }
        }
        // The getters as whole words: `in_process_get_cloud_sync_settings` is a
        // different function.
        let whole = |needle: &str| -> Vec<usize> {
            bare.match_indices(needle)
                .filter(|(i, _)| {
                    !i.checked_sub(1)
                        .map(|j| bare.as_bytes()[j])
                        .is_some_and(ident)
                })
                .map(|(i, _)| i)
                .collect()
        };
        offsets.extend(whole("get_cloud_sync_enabled"));
        if !owns_settings_getter {
            offsets.extend(whole("get_cloud_sync_settings("));
        }
        for (i, _) in code.match_indices("[\"cloud_sync_enabled\"]") {
            let before = i.checked_sub(1).map(|j| code.as_bytes()[j]);
            if before.is_some_and(|c| ident(c) || c == b')' || c == b']') {
                offsets.push(i);
            }
        }
        let mut lines: Vec<usize> = offsets
            .into_iter()
            .map(|i| src.get(..i).map_or(0, |pre| pre.matches('\n').count()) + 1)
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// M3: a content send whose session tenant could not be resolved is
    /// judged by the STRICTEST verdict across the default scope and every
    /// bound tenant — never the default scope alone.
    #[test]
    fn an_unresolved_session_takes_the_strictest_bound_tenant() {
        let base = "http://coord.example";
        let a = Uuid::from_u128(0xa3);
        let b = Uuid::from_u128(0xb3);
        let state = EgressState::new(None, None, ScopeKey::new(base, Some(a)));
        let decide = |t: Uuid, level: Level| {
            state.apply_coord_answer(
                &ScopeKey::new(base, Some(t)),
                Flow::TerminalStream,
                CoordAnswer::Authoritative(Decision {
                    level,
                    decided_by: CoordOrigin::TenantRow,
                }),
            )
        };
        decide(a, Level::On);
        decide(b, Level::Off);
        state.record_bound_enumeration(vec![a, b], true);
        let flow = Flow::TerminalStream;
        assert!(state.permit_session(flow, SessionScope::Tenant(a)).allowed);
        assert!(
            state
                .permit_session(flow, SessionScope::DeviceDefault)
                .allowed
        );
        let unresolved = state.permit_session(flow, SessionScope::Unresolved);
        assert!(
            !unresolved.allowed,
            "B's off refuses an unattributable send"
        );
        decide(b, Level::On);
        assert!(state.permit_session(flow, SessionScope::Unresolved).allowed);
        // A bound tenant nobody has answered for is UNKNOWN, which refuses too.
        let c = Uuid::from_u128(0xc3);
        state.record_bound_enumeration(vec![c], true);
        assert!(!state.permit_session(flow, SessionScope::Unresolved).allowed);
    }

    /// Re-review M3: an EMPTY bound set refuses unattributable content —
    /// unless a complete enumeration found the device unbound — and a failed
    /// or partial enumeration never shrinks the set (merge, not replace).
    #[test]
    fn an_empty_bound_set_refuses_and_enumerations_merge() {
        let base = "http://coord.example";
        let flow = Flow::TerminalStream;
        let fresh = || EgressState::new(None, None, ScopeKey::new(base, None));

        let never = fresh();
        assert!(
            !never.permit_session(flow, SessionScope::Unresolved).allowed,
            "nothing enumerated yet: refuse"
        );
        let failed = fresh();
        failed.record_bound_enumeration(Vec::new(), false);
        assert!(
            !failed
                .permit_session(flow, SessionScope::Unresolved)
                .allowed,
            "a failed enumeration is not 'no tenants'"
        );
        let unbound = fresh();
        unbound.record_bound_enumeration(Vec::new(), true);
        assert!(
            unbound
                .permit_session(flow, SessionScope::Unresolved)
                .allowed,
            "a device with no credentials has no tenant to protect"
        );

        let a = Uuid::from_u128(0xa6);
        let b = Uuid::from_u128(0xb6);
        let merged = fresh();
        merged.record_bound_enumeration(vec![a], true);
        merged.record_bound_enumeration(Vec::new(), false);
        assert_eq!(
            merged.bound_tenants(),
            vec![a],
            "a failed tick keeps the set"
        );
        merged.record_bound_enumeration(Vec::new(), true);
        assert!(
            !merged.bound_set().unbound_device,
            "a set with tenants is never 'unbound'"
        );
        merged.record_bound_enumeration(vec![b], true);
        assert_eq!(merged.bound_tenants(), vec![a, b], "enumerations merge");
        // Never answered: both are UNKNOWN and refuse.
        assert!(
            !merged
                .permit_session(flow, SessionScope::Unresolved)
                .allowed
        );
        // The module door: an empty faked set (not unbound) refuses too.
        let _on = test_support::pin(flow, Level::On);
        let _bound = test_support::fake_bound_tenants(Vec::new());
        assert!(!permit_session(flow, SessionScope::Unresolved).allowed);
    }

    /// Re-review C1: ownership is decided on CANONICAL paths — `..` and
    /// symlinks cannot borrow another tenant's directory.
    #[test]
    fn path_ownership_is_decided_on_canonical_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let a_dir = root.join("wt-a");
        let b_dir = root.join("wt-b");
        std::fs::create_dir_all(a_dir.join("src")).unwrap();
        std::fs::create_dir_all(b_dir.join("src")).unwrap();
        std::fs::write(b_dir.join("src/secret.rs"), "b").unwrap();
        std::fs::write(a_dir.join("src/lib.rs"), "a").unwrap();
        let a = Uuid::from_u128(0xa7);
        let b = Uuid::from_u128(0xb7);
        let (a_s, b_s) = (a.to_string(), b.to_string());
        let a_path = a_dir.to_string_lossy().to_string();
        let b_path = b_dir.to_string_lossy().to_string();
        let sessions = [
            (a_path.as_str(), Some(a_s.as_str())),
            (b_path.as_str(), Some(b_s.as_str())),
        ];
        let scope =
            |p: &std::path::Path| scope_for_path(&p.to_string_lossy(), sessions.iter().copied());
        assert_eq!(scope(&a_dir.join("src/lib.rs")), SessionScope::Tenant(a));
        // `..` out of A's tree into B's file is B's file.
        assert_eq!(
            scope(&a_dir.join("src/../../wt-b/src/secret.rs")),
            SessionScope::Tenant(b)
        );
        // A symlink inside A's tree pointing at B's file is B's file.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(b_dir.join("src/secret.rs"), a_dir.join("src/link.rs"))
                .unwrap();
            assert_eq!(scope(&a_dir.join("src/link.rs")), SessionScope::Tenant(b));
        }
        // A path that does not exist cannot be canonicalized: unresolved.
        assert_eq!(
            scope(&a_dir.join("src/missing.rs")),
            SessionScope::Unresolved
        );
    }

    /// Re-review C1: on Windows, ownership keys drop the verbatim `\\?\`
    /// prefix and compare case-insensitively; POSIX keys stay as they are.
    #[test]
    fn windows_ownership_keys_are_case_and_prefix_insensitive() {
        let k = |raw: &str| normalize_ownership_key(raw, true);
        assert_eq!(k(r"\\?\C:\Work\Repo"), k(r"c:\work\repo"));
        assert_eq!(k(r"\\?\UNC\Server\Share\x"), k(r"\\server\share\X"));
        assert_eq!(k("C:/Work/Repo"), k(r"c:\work\repo"));
        assert_ne!(
            normalize_ownership_key("/Work/Repo", false),
            normalize_ownership_key("/work/repo", false),
            "POSIX paths stay case-sensitive"
        );
    }

    /// M3: the strictest rule's set is seeded from the store before the first
    /// poll, so a restart does not open unattributable sends meanwhile.
    #[test]
    fn the_bound_set_is_seeded_from_the_store_before_the_first_poll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        let base = "http://coord.example";
        let a = Uuid::from_u128(0xa4);
        let b = Uuid::from_u128(0xb4);
        let first = EgressState::new(Some(path.clone()), None, ScopeKey::new(base, Some(a)));
        first.apply_coord_answer(
            &ScopeKey::new(base, Some(b)),
            Flow::TranscriptSync,
            CoordAnswer::Authoritative(Decision {
                level: Level::Off,
                decided_by: CoordOrigin::TenantRow,
            }),
        );
        let restarted = EgressState::new(Some(path), None, ScopeKey::new(base, Some(a)));
        assert_eq!(restarted.bound_tenants(), vec![b]);
        assert!(
            !restarted
                .permit_session(Flow::TranscriptSync, SessionScope::Unresolved)
                .allowed
        );
    }

    /// M3: the module-level door used by every content call site applies the
    /// same rule over the faked bound set and pins.
    #[test]
    fn the_session_door_refuses_an_unresolved_send_when_any_bound_tenant_is_off() {
        let t = Uuid::from_u128(0x7e7a_0006);
        let _on = test_support::pin(Flow::TranscriptSync, Level::On);
        let _off = test_support::pin_for(Flow::TranscriptSync, t, Level::Off);
        let _bound = test_support::fake_bound_tenants(vec![t]);
        assert!(!permit_session(Flow::TranscriptSync, SessionScope::Unresolved).allowed);
        assert!(!transcript_sync_gate_session(SessionScope::from_lookup(None)).is_open());
        assert!(permit_session(Flow::TranscriptSync, SessionScope::DeviceDefault).allowed);
    }

    /// Code-mirror relay reads: a path belongs to the session working in its
    /// closest enclosing directory; disagreement or no session is unresolved.
    #[test]
    fn a_path_is_owned_by_the_session_working_closest_above_it() {
        let tmp = tempfile::tempdir().unwrap();
        let w = std::fs::canonicalize(tmp.path()).unwrap();
        for f in [
            "root/wt-a/src/lib.rs",
            "root/other/x",
            "root/wt-ab/x",
            "shared/x",
            "plain/x",
            "bad/x",
            "elsewhere/x",
        ] {
            let p = w.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x").unwrap();
        }
        let d = |rel: &str| w.join(rel).to_string_lossy().to_string();
        let a = Uuid::from_u128(0xa5).to_string();
        let b = Uuid::from_u128(0xb5).to_string();
        let dirs = [d("root"), d("root/wt-a"), d("shared"), d("plain"), d("bad")];
        let sessions = [
            (dirs[0].as_str(), Some(b.as_str())),
            (dirs[1].as_str(), Some(a.as_str())),
            (dirs[2].as_str(), Some(a.as_str())),
            (dirs[2].as_str(), Some(b.as_str())),
            (dirs[3].as_str(), None),
            (dirs[4].as_str(), Some("not-a-uuid")),
            ("/no/such/session/dir", Some(a.as_str())),
        ];
        let scope = |rel: &str| scope_for_path(&d(rel), sessions.iter().copied());
        assert_eq!(
            scope("root/wt-a/src/lib.rs"),
            SessionScope::Tenant(Uuid::from_u128(0xa5))
        );
        assert_eq!(
            scope("root/other/x"),
            SessionScope::Tenant(Uuid::from_u128(0xb5))
        );
        assert_eq!(
            scope("root/wt-ab/x"),
            SessionScope::Tenant(Uuid::from_u128(0xb5)),
            "component-wise"
        );
        assert_eq!(scope("shared/x"), SessionScope::Unresolved);
        assert_eq!(scope("plain/x"), SessionScope::DeviceDefault);
        assert_eq!(scope("bad/x"), SessionScope::Unresolved);
        assert_eq!(scope("elsewhere/x"), SessionScope::Unresolved);
        assert_eq!(
            scope_for_path("relative/x", sessions.iter().copied()),
            SessionScope::Unresolved
        );
    }

    /// Re-review 9: the transcript and code-mirror paths that know their
    /// session ask in that session's tenant (the relay and remote-attach
    /// paths are covered behaviourally in their own modules' `egress_tests`).
    #[test]
    fn session_aware_paths_ask_in_the_sessions_tenant() {
        let squash = |src: &str| src.split_whitespace().collect::<String>();
        let tailer = squash(include_str!("session/session_transcript_tailer.rs"));
        assert!(
            tailer.contains(&squash(
                "transcript_sync_gate_session(crate::egress::SessionScope::from_lookup(self.registrar.recorded_tenant(session_key),))"
            )),
            "the tailer asks in the session's recorded tenant"
        );
        assert!(
            tailer.contains(&squash("self.transcript_sync_open(session_key),")),
            "the tailer's own admit uses the session-tenant gate"
        );
        let watcher = squash(include_str!("terminal/transcript_watcher.rs"));
        assert!(
            watcher.contains(&squash("t.transcript_sync_open(&session_id)")),
            "the watcher asks the tailer's session-tenant gate"
        );
        let emitter = squash(include_str!("session/transcript_emitter.rs"));
        assert!(
            emitter.contains(&squash(
                "transcript_sync_gate_session(crate::egress::SessionScope::from_lookup(self.registrar.recorded_tenant(session_key),))"
            )),
            "the emitter asks in the session's recorded tenant"
        );
        let pusher = squash(include_str!("agent_pusher/mod.rs"));
        assert!(
            pusher.contains(&squash(
                "permit_or_count_for(crate::egress::Flow::CodeMirror, crate::auth::jwt_tenant_claim(token),"
            )),
            "the code mirror asks in the agent token's tenant"
        );
    }

    /// L8: the scanner does not cut a line at a `//` inside a string, and
    /// catches destructuring, JSON-index and settings-command reads.
    #[test]
    fn the_consent_scanner_sees_through_strings_and_catches_every_read_shape() {
        let src = r##"
let url = "http://x"; let on = s.cloud_sync_enabled;
let Settings { cloud_sync_enabled, .. } = load();
let v = body["cloud_sync_enabled"];
let r = crate::commands::cloud_sync_settings::get_cloud_sync_settings(); in_process_get_cloud_sync_settings(&a);
// a comment naming cloud_sync_enabled is fine
let schema = r#"{"required":["cloud_sync_enabled"]}"#;
let label = "blocked by cloud_sync_enabled";
"##;
        assert_eq!(consent_read_lines(src, false), vec![2, 3, 4, 5]);
        assert_eq!(
            consent_read_lines(src, true),
            vec![2, 3, 4],
            "the defining module may call its own command"
        );
    }

    /// No egress path may read the user consent bare: every read outside the
    /// settings plumbing, this module and the reporting surfaces goes through
    /// [`transcript_sync_permitted`], so a new egress path cannot skip the
    /// tenant switch.
    #[test]
    fn no_bare_cloud_sync_reads_outside_the_allowed_files() {
        const ALLOWED: &[&str] = &[
            "settings.rs",
            "egress.rs",
            "commands/cloud_sync_settings.rs",
        ];
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![root.clone()];
        let mut scanned = 0usize;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                scanned += 1;
                if ALLOWED.contains(&rel.as_str()) {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                let module_owns_settings_getter = rel == "commands/cloud_sync_settings.rs";
                for n in consent_read_lines(&src, module_owns_settings_getter) {
                    offenders.push(format!("{rel}:{n}"));
                }
            }
        }
        assert!(
            scanned > 100,
            "the scan must actually walk src/ ({scanned})"
        );
        assert!(
            offenders.is_empty(),
            "bare get_cloud_sync_enabled reads outside the allowed files — use \
             crate::egress::transcript_sync_permitted(): {offenders:?}"
        );
    }
}
