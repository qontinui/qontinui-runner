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
//! for this tenant ([`CoordAnswer::NoOpinion`]): it clears rungs 1 and 2 for
//! that scope (a deleted tenant row must not live on in the store) and lets the
//! machine profile, then the product default, answer. It is never persisted and
//! never outranks a profile's `off`. A no-row answer with no `default_source`
//! at all comes from a coord that predates the egress family
//! ([`CoordAnswer::NotAnEgressAnswer`]) — its level is that coord's generic
//! unknown-domain `off`, so it changes nothing.
//!
//! ## Scope: per coord deployment and per tenant
//!
//! Answers are keyed by [`ScopeKey`] — the coord base URL plus the tenant —
//! both in memory and in the store, so a runner re-pointed at another coord,
//! or re-paired into another tenant, never inherits the previous one's
//! switches. A call site that knows the tenant of the session it is sending
//! for asks [`permit_for`] with it (the session-output drain, the terminal
//! output pipe, transcript-bind); everything else asks for the device's
//! default tenant.
//!
//! **The remaining multi-tenant limit.** The poller asks coord with the
//! device's DEFAULT credential, so only the default tenant's switches are
//! polled. A session owned by another tenant this device is bound to is
//! evaluated against that tenant's last persisted answer (from a process in
//! which it was the default), else the machine profile, else the product
//! default — not against a live answer. `/health` `egress.scope` names the
//! polled scope and states this.
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
//! - code mirror: `agent_pusher::push_one`;
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

/// Store schema version. A file carrying another value is treated as absent.
const STORE_SCHEMA: u32 = 2;

/// The `resolved_scope` coord answers when no policy row exists.
const RESOLVED_SCOPE_NONE: &str = "none";

/// The `default_source` of a self-hosted coord's egress default.
const DEFAULT_SOURCE_DEPLOYMENT_PROFILE: &str = "deployment_profile";

/// The `default_source` of coord's product default — coord has no opinion.
const DEFAULT_SOURCE_PRODUCT: &str = "product";

/// What `/health` says about the polled scope's limit.
const SCOPE_NOTE: &str = "only the device's default tenant is polled; a session of another \
     bound tenant is evaluated against that tenant's persisted answer, else the machine \
     profile, else the product default";

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
}

impl CoordOrigin {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            CoordOrigin::TenantRow => "tenant_row",
            CoordOrigin::DeploymentProfile => "deployment_profile",
        }
    }
}

/// One authoritative coord decision for one flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Decision {
    pub(crate) level: Level,
    pub(crate) decided_by: CoordOrigin,
}

/// Which C6 rung produced a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelSource {
    Coord(CoordOrigin),
    Persisted(CoordOrigin),
    Profile,
    ProductDefault,
}

impl LevelSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            LevelSource::Coord(_) => "coord",
            LevelSource::Persisted(_) => "persisted",
            LevelSource::Profile => "profile",
            LevelSource::ProductDefault => "product_default",
        }
    }

    /// What on coord's side decided it, for the two coord rungs.
    pub(crate) const fn decided_by(self) -> Option<CoordOrigin> {
        match self {
            LevelSource::Coord(o) | LevelSource::Persisted(o) => Some(o),
            LevelSource::Profile | LevelSource::ProductDefault => None,
        }
    }
}

/// What [`permit`] answers for one flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EgressVerdict {
    pub(crate) allowed: bool,
    pub(crate) source: LevelSource,
}

/// The C6 ladder. PURE: the first rung holding a level wins.
pub(crate) fn resolve(
    coord: Option<Decision>,
    persisted: Option<Decision>,
    profile: Option<Level>,
) -> EgressVerdict {
    let (level, source) = if let Some(d) = coord {
        (d.level, LevelSource::Coord(d.decided_by))
    } else if let Some(d) = persisted {
        (d.level, LevelSource::Persisted(d.decided_by))
    } else if let Some(l) = profile {
        (l, LevelSource::Profile)
    } else {
        (Level::On, LevelSource::ProductDefault)
    };
    EgressVerdict {
        allowed: level == Level::On,
        source,
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
/// unreadable or unparseable, or that names an active profile it does not
/// define, reads `off`: the machine was configured, and a configuration this
/// runner cannot read is never an authorisation to send.
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
    /// tenant. Clears rungs 1 and 2 for the scope; never persisted.
    NoOpinion,
    /// A no-row answer from a coord that does not know the egress family (no
    /// `default_source`), or names a default this runner cannot interpret.
    /// Changes nothing.
    NotAnEgressAnswer,
}

/// Classify a 2xx `GET /coord/fleet-policy?domain=egress_*` body. PURE.
///
/// Within an authoritative answer, an unreadable level is `off`: coord said
/// something about this flow, and a level we cannot identify is never an
/// authorisation to send.
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

type Decisions = [Option<Decision>; 6];
type ScopedDecisions = HashMap<ScopeKey, Decisions>;

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
    /// domain → decision. Unknown domains and unreadable entries are dropped.
    levels: std::collections::BTreeMap<String, Value>,
}

/// Decode a store file. PURE. A corrupt file, a foreign schema, unknown
/// domains and unreadable entries all read as absent — never a panic, never a
/// guess.
fn decode_store(raw: &str) -> ScopedDecisions {
    let mut out = ScopedDecisions::new();
    let file: StoreFile = match serde_json::from_str(raw) {
        Ok(f) => f,
        Err(e) => {
            warn!("egress: persisted levels unreadable — treating as absent: {e}");
            return out;
        }
    };
    if file.schema != STORE_SCHEMA {
        warn!(
            "egress: persisted levels carry schema {} (expected {STORE_SCHEMA}) — treating as \
             absent",
            file.schema
        );
        return out;
    }
    for scope in file.scopes {
        let mut decisions: Decisions = [None; 6];
        for (domain, entry) in &scope.levels {
            let (Some(flow), Ok(decision)) = (
                Flow::from_domain(domain),
                serde_json::from_value::<Decision>(entry.clone()),
            ) else {
                continue;
            };
            decisions[flow.index()] = Some(decision);
        }
        out.insert(
            ScopeKey::new(&scope.key.coord_base, scope.key.tenant_id),
            decisions,
        );
    }
    out
}

/// Encode a store file. PURE apart from the timestamp.
fn encode_store(scopes: &ScopedDecisions) -> Vec<u8> {
    let mut entries: Vec<StoreScope> = scopes
        .iter()
        .filter(|(_, d)| d.iter().any(Option::is_some))
        .map(|(key, decisions)| StoreScope {
            key: key.clone(),
            levels: Flow::ALL
                .into_iter()
                .filter_map(|f| {
                    let d = decisions[f.index()]?;
                    Some((f.domain().to_string(), serde_json::to_value(d).ok()?))
                })
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
fn load_store(path: &Path) -> ScopedDecisions {
    match std::fs::read_to_string(path) {
        Ok(raw) => decode_store(&raw),
        Err(_) => ScopedDecisions::new(),
    }
}

/// Write the store atomically, creating the parent directory explicitly here —
/// the one writer — rather than as a side effect of resolving the path (the
/// `persist_briefings` precedent). Best-effort: a failure costs durability
/// across the next restart, never the in-memory state.
fn persist_store(path: &Path, scopes: &ScopedDecisions) {
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
    /// Rung 1 — this process's authoritative coord answers, per scope.
    coord: RwLock<ScopedDecisions>,
    /// Rung 2 — what the store held at boot, updated as answers are persisted.
    persisted: RwLock<ScopedDecisions>,
    /// The scope the poller asks about: the device's default tenant at the
    /// coord it talks to. A call that names no tenant is evaluated here.
    current: RwLock<ScopeKey>,
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
        let persisted = store_path.as_deref().map(load_store).unwrap_or_default();
        Self {
            coord: RwLock::new(ScopedDecisions::new()),
            persisted: RwLock::new(persisted),
            current: RwLock::new(current),
            profile,
            store_path,
            refused: Default::default(),
            telemetry_boot: AtomicU8::new(0),
        }
    }

    fn lookup(map: &RwLock<ScopedDecisions>, key: &ScopeKey, flow: Flow) -> Option<Decision> {
        map.read()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .and_then(|d| d[flow.index()])
    }

    /// The polled scope.
    pub(crate) fn current_scope(&self) -> ScopeKey {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Move the polled scope (the poller, each tick).
    pub(crate) fn set_current_scope(&self, key: ScopeKey) {
        *self.current.write().unwrap_or_else(|p| p.into_inner()) = key;
    }

    /// The scope for a call: `tenant` when the caller knows it, else the
    /// device's default; always at the coord the poller talks to.
    fn scope_for(&self, tenant: Option<Uuid>) -> ScopeKey {
        let current = self.current_scope();
        ScopeKey {
            tenant_id: tenant.or(current.tenant_id),
            coord_base: current.coord_base,
        }
    }

    /// The C6 verdict for `flow` in `tenant`'s scope (`None` = the device's
    /// default tenant). Lock-only, safe on any path.
    pub(crate) fn permit_for(&self, flow: Flow, tenant: Option<Uuid>) -> EgressVerdict {
        let key = self.scope_for(tenant);
        resolve(
            Self::lookup(&self.coord, &key, flow),
            Self::lookup(&self.persisted, &key, flow),
            self.profile,
        )
    }

    /// [`Self::permit_for`] in the default tenant's scope.
    pub(crate) fn permit(&self, flow: Flow) -> EgressVerdict {
        self.permit_for(flow, None)
    }

    /// Apply one classified coord answer for `flow` in scope `key`.
    ///
    /// - authoritative: rung 1 holds it, and the store is rewritten when (and
    ///   only when) the persisted value changes — a steady state rewrites
    ///   nothing;
    /// - no opinion (the product default): rungs 1 and 2 are cleared for the
    ///   scope, so a deleted tenant row does not live on in the store and the
    ///   profile answers;
    /// - not an egress answer: nothing.
    pub(crate) fn apply_coord_answer(&self, key: &ScopeKey, flow: Flow, answer: CoordAnswer) {
        let i = flow.index();
        let next = match answer {
            CoordAnswer::Authoritative(d) => Some(d),
            CoordAnswer::NoOpinion => None,
            CoordAnswer::NotAnEgressAnswer => return,
        };
        self.coord
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(key.clone())
            .or_insert([None; 6])[i] = next;
        let snapshot = {
            let mut persisted = self.persisted.write().unwrap_or_else(|p| p.into_inner());
            let current = persisted.get(key).and_then(|d| d[i]);
            if current == next {
                return;
            }
            persisted.entry(key.clone()).or_insert([None; 6])[i] = next;
            persisted.clone()
        };
        if let Some(path) = &self.store_path {
            persist_store(path, &snapshot);
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

    /// The `/health` `egress` object over this state: `scope` (the polled
    /// coord + tenant, and the multi-tenant limit) plus one entry per flow.
    pub(crate) fn health_json(&self) -> Value {
        let current = self.current_scope();
        let mut out = serde_json::Map::new();
        out.insert(
            "scope".to_string(),
            json!({
                "coord_base": current.coord_base,
                "tenant_id": current.tenant_id,
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

/// The scope a fresh process starts on: the coord base this runner resolves
/// and the device's declared default tenant.
#[cfg(not(test))]
fn production_scope() -> ScopeKey {
    let (base, _source) = qontinui_runner_lib::profiles::coord_base_with_source();
    ScopeKey::new(
        &base,
        crate::session::dual_write::resolve_active_tenant_id(),
    )
}

/// The process-global state. Test builds never touch the real config dir or
/// profile: their global starts empty (every flow at the product default), and
/// tests pin levels per thread through [`test_support::pin`].
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
        #[cfg(test)]
        {
            EgressState::new(None, None, ScopeKey::new("http://coord.test", None))
        }
    })
}

/// The C6 verdict for `flow` in `tenant`'s scope (`None` = the device's
/// default tenant). Lock-only — safe from synchronous spawn paths,
/// keystroke-rate relay handlers and the boot sequence.
pub(crate) fn permit_for(flow: Flow, tenant: Option<Uuid>) -> EgressVerdict {
    #[cfg(test)]
    if let Some(level) = test_support::pinned(flow) {
        return EgressVerdict {
            allowed: level == Level::On,
            source: LevelSource::Coord(CoordOrigin::TenantRow),
        };
    }
    state().permit_for(flow, tenant)
}

/// [`permit_for`] in the device's default tenant's scope.
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

/// [`permit_or_count_for`] in the default tenant's scope.
pub(crate) fn permit_or_count(flow: Flow) -> bool {
    permit_or_count_for(flow, None)
}

/// Record coord's classified answer for `flow` in scope `key` (the poller's
/// write door).
pub(crate) fn apply_coord_answer(key: &ScopeKey, flow: Flow, answer: CoordAnswer) {
    state().apply_coord_answer(key, flow, answer);
}

/// Move the polled scope (the poller's write door, each tick).
pub(crate) fn set_current_scope(key: ScopeKey) {
    state().set_current_scope(key);
}

/// Transcript sync's full consent, with the reason when it is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptGate {
    Open,
    /// The user's own `cloud_sync_enabled` toggle is off.
    UserConsentOff,
    /// The tenant's `egress_transcript_sync` switch is off.
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

/// Transcript sync's gate for a session of `tenant` (`None` = the device's
/// default tenant): the tenant's `egress_transcript_sync` AND the user's own
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

/// [`transcript_sync_gate_for`] in the default tenant's scope, as a bool.
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

/// The refusal frame a relay handler answers instead of sending `flow`'s
/// bytes, typed as the reply the caller is waiting for (`terminal_response`,
/// `chat_output`). `request_id` / `terminal_id` / `session_id` are echoed so
/// the web side can correlate it; the console renders `message`.
pub(crate) fn egress_refusal_frame(reply_type: &str, flow: Flow, data: &Value) -> Value {
    let message = match flow {
        Flow::TerminalStream => "Terminal streaming is off for this project",
        Flow::TranscriptSync => "Transcript sync is off for this project",
        Flow::CodeMirror => "The code mirror is off for this project",
        Flow::Telemetry => "Telemetry is off for this project",
        Flow::UpdateCheck => "Update checks are off for this project",
        Flow::SkillMirror => "The skill mirror is off for this project",
    };
    json!({
        "type": reply_type,
        "error": "egress_off",
        "flow": flow.key(),
        "domain": flow.domain(),
        "message": message,
        "request_id": data.get("request_id").cloned().unwrap_or(Value::Null),
        "terminal_id": data.get("terminal_id").cloned().unwrap_or(Value::Null),
        "session_id": data.get("session_id").cloned().unwrap_or(Value::Null),
    })
}

/// The refusal frame a relay `terminal_*` handler answers instead of streaming.
pub(crate) fn terminal_refusal_frame(data: &Value) -> Value {
    egress_refusal_frame("terminal_response", Flow::TerminalStream, data)
}

/// `GET /health` `egress`: the polled `scope`, and every flow's `{allowed,
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

    thread_local! {
        static PINS: RefCell<[Option<Level>; 6]> = const { RefCell::new([None; 6]) };
    }

    pub(crate) fn pinned(flow: Flow) -> Option<Level> {
        PINS.with(|p| p.borrow()[flow.index()])
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
            Some(Decision {
                level,
                decided_by: CoordOrigin::TenantRow,
            })
        };
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
        ];
        for ((c, p, prof), (allowed, source)) in cases {
            assert_eq!(
                resolve(c, p, prof),
                EgressVerdict { allowed, source },
                "coord={c:?} persisted={p:?} profile={prof:?}"
            );
        }
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
            LevelSource::ProductDefault
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
        // A flow coord never answered falls to the profile rung.
        assert_eq!(
            second.permit(Flow::Telemetry),
            EgressVerdict {
                allowed: true,
                source: LevelSource::Profile
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
            LevelSource::ProductDefault,
            "tenant A's off must not apply to tenant B's session"
        );
        // Re-paired into tenant B: the default scope moves with it.
        st.set_current_scope(key(Some(TENANT_B)));
        assert!(st.permit(Flow::TerminalStream).allowed);
        // Re-pointed at another coord: none of the first coord's answers.
        let other = EgressState::new(
            Some(path.clone()),
            None,
            ScopeKey::new("https://other-coord.example", Some(TENANT_A)),
        );
        assert_eq!(
            other.permit(Flow::TerminalStream).source,
            LevelSource::ProductDefault
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
        assert!(decode_store("{not json").is_empty());
        assert!(decode_store(r#"{"schema":1,"written_at":"x","levels":{}}"#).is_empty());
        let mixed = decode_store(
            r#"{"schema":2,"written_at":"x","scopes":[{"coord_base":"http://c/","tenant_id":null,
                "levels":{"egress_telemetry":{"level":"off","decided_by":"tenant_row"},
                "egress_unknown":{"level":"off","decided_by":"tenant_row"},
                "egress_code_mirror":{"level":"maybe","decided_by":"tenant_row"}}}]}"#,
        );
        let d = mixed[&ScopeKey::new("http://c", None)];
        assert_eq!(
            d[Flow::Telemetry.index()].map(|d| d.level),
            Some(Level::Off)
        );
        assert_eq!(d[Flow::CodeMirror.index()], None);
        // A missing file creates nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join(STORE_FILE);
        assert!(load_store(&path).is_empty());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let mut scopes = ScopedDecisions::new();
        let mut a: Decisions = [None; 6];
        a[Flow::SkillMirror.index()] = Some(Decision {
            level: Level::Off,
            decided_by: CoordOrigin::DeploymentProfile,
        });
        let mut b: Decisions = [None; 6];
        b[Flow::UpdateCheck.index()] = Some(Decision {
            level: Level::On,
            decided_by: CoordOrigin::TenantRow,
        });
        scopes.insert(key(Some(TENANT_A)), a);
        scopes.insert(key(None), b);
        let raw = String::from_utf8(encode_store(&scopes)).unwrap();
        assert_eq!(decode_store(&raw), scopes);
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
        assert!(v["scope"]["note"]
            .as_str()
            .unwrap()
            .contains("default tenant"));
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
        assert_eq!(v["telemetry"]["source"], "profile");
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
                for (n, line) in src.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    // The getter, AND a bare `.cloud_sync_enabled` field read
                    // (a struct carrying the raw user toggle past the gate).
                    let field_read = code.match_indices(".cloud_sync_enabled").any(|(i, m)| {
                        !code
                            .get(i + m.len()..)
                            .and_then(|rest| rest.chars().next())
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    });
                    if code.contains("get_cloud_sync_enabled") || field_read {
                        offenders.push(format!("{rel}:{}", n + 1));
                    }
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
