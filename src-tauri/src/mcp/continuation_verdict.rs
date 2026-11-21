//! Stop-hook continuation verdict (plan `2026-07-17-session-autonomy-fabric.md`,
//! Phase 1 — the assumption killer).
//!
//! The runner provisions a Claude Code `Stop` hook on every shim-launched
//! session (`session::claude_hook` materializes `claude_stop_hook.sh` into the
//! shared `--settings` carrier). When the agent is about to end a turn, the
//! hook POSTs the Stop payload to
//! `POST /sessions/{id}/continuation-verdict` (`mcp::sessions::routes`), and
//! THIS module computes the deterministic verdict:
//!
//! - **block + prompt** — only when the session's coord `session_status` is
//!   `working` or ABSENT (coord reachable but no status recorded) AND the
//!   kill-switch flag is `on`.
//! - **allow** — for every terminal status (`done`/`finished`/`waiting_human`/
//!   `blocked`), whenever coord is unreachable (FAIL-OPEN — a coord outage
//!   must never trap a session at turn-end), and whenever a loop guard trips.
//!
//! ## Loop guards (robustness)
//!
//! 1. `stop_hook_active` in the hook input ⇒ allow. A continuation the hook
//!    chain already forced is never re-blocked (the hook script also
//!    short-circuits locally; this is the authoritative leg).
//! 2. Hourly cap: at most [`DEFAULT_HOURLY_CAP`] (env-tunable) hook-driven
//!    continuations per session per rolling hour, counted in a registry-style
//!    process-global counter. Only ENFORCED blocks consume the cap.
//! 3. Fail-open on ANY error — missing JWT, unresolved coord base, transport
//!    error, undecodable body, poisoned lock: all collapse to allow.
//!
//! ## Kill-switch flag
//!
//! `QONTINUI_STOP_HOOK_CONTINUATION=off|observe|on`, default **off** (the
//! project's tri-state ramp convention). `observe` computes + logs the
//! would-be verdict and returns allow; `on` enforces. Unknown values read as
//! `off` (fail-safe).
//!
//! ## Gate continuations are armed by default
//!
//! An UNSET flag ([`FlagReading::Unset`]) is not the same reading as an
//! explicit `off`. For a **gate continuation** — a terminal the runner's own
//! `ContinuationRegistry` holds with a `gate_id`
//! ([`crate::agent_runtime::live_gate_continuation_gate_id`]) — an unset flag
//! resolves to `on`: those sessions are the unattended population with no
//! operator to close them, and they idled at their prompt holding a
//! continuation slot forever (plan
//! `2026-10-28-finished-gate-continuations-never-free-a-slot-so-the-box-deadlocks-at-the-continuation-cap`,
//! Phase 2). An explicit `off` stays the off-switch for them too. Every other
//! session keeps the dark default.
//!
//! Scoping is by the registry lookup on the route's path id (the runner
//! TERMINAL id), never by title and never by the session's environment, which
//! this process cannot read ([`mode_for_terminal`]).
//!
//! ## Coord fetches + cache
//!
//! Coord reads are cached in process-global TTL caches (45s, mirroring
//! `fleet_policy_poller`'s poll-into-process-global-cache posture — NOT ETag):
//!
//! - **session status** — the device-authed bulk work-axis door
//!   `GET {coord}/coord/sessions/work-status?claude_code_session_ids=<id>`
//!   ([`crate::mcp::session_work_status::fetch_one`]), keyed by the Claude
//!   session id the Stop payload carries. This read USED to go to the
//!   session-identity resolver card (`/coord/agent-sessions/{key}`), which is
//!   operator-gated: measured 2026-10-28 it answered 401 to the runner's
//!   device JWT on three probes while the work-status door answered 200 on the
//!   same token, so every verdict fail-opened to `allow` and the feature was
//!   inert even where armed (coord finding 4eef6693). A row with no
//!   `session_status` reads as ABSENT (block-eligible). An id coord names in
//!   its `unknown` bucket, a payload with no session id, and a degraded read
//!   are all `Unreachable` ⇒ allow.
//! - **continuation rules** — `GET {coord}/coord/agent-prompt-documents/
//!   continuation_rules/session-continuation`: coord's versioned
//!   `prompt_documents` store (Phase 2), which serves the tenant's editable
//!   umbrella prompt. Read over the DEVICE-authed door, not the operator
//!   `/coord/prompt-documents/*` one — the latter 403s a device JWT, and
//!   because this fetch fail-opens that 403 would silently pin every session
//!   to [`DEFAULT_CONTINUATION_RULES`] forever (see
//!   [`continuation_rules_url`]). Any failure falls back to
//!   [`DEFAULT_CONTINUATION_RULES`].
//!
//! `QONTINUI_STOP_HOOK_STATUS_OVERRIDE` is a dev/E2E lever: when set, the
//! status fetch short-circuits to that value (`absent` ⇒ reachable-but-no-
//! status) without touching coord, so the block path is live-verifiable
//! before the coord read surface exists. It is logged loudly on every use.
//!
//! ## Whose tenant — the SESSION's, decided once per Stop
//!
//! Plan `2026-11-13-runner-stop-hook-continuation-rules-and-compliance-use-the-device-default-tenant`.
//! On a device bound to several tenants, both reads above — and the
//! compliance config read and POST [`crate::mcp::session_compliance`] makes on
//! the same Stop — used to present the device DEFAULT credential, so a session
//! working another tenant's repo had its work axis read where it has no row
//! (`unknown` ⇒ allow: the continuation arm never fired for it) and was handed
//! the default tenant's rules. Now [`continuation_verdict`] decides the
//! session's tenant ONCE ([`crate::mcp::policy_tenant::route_policy_target`]
//! under [`crate::mcp::policy_tenant::STOP_TENANT_BUDGET`]) and hands that one
//! target to every reader, which presents that tenant's credential through
//! `coord_http::coord_get_for` / `coord_post_for` and refuses to send anything
//! whose bearer does not claim it ([`crate::mcp::policy_tenant::build_checked`]).
//! An UNKNOWN tenant (multi-bound, unresolvable) reads nothing: the status is
//! UNKNOWN (allow) and the rules are the built-in default. The caches are keyed
//! on the tenant as well, and an UNKNOWN decision reads and writes none of them.
//! The verdict names the tenant and the rules version it used
//! ([`VerdictResponse::tenant`], [`VerdictResponse::rules_version`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::mcp::policy_tenant::{self, PolicyTarget};

// ===========================================================================
// Flag + constants
// ===========================================================================

/// Tri-state kill-switch env flag. `off` (default) | `observe` | `on`.
pub const FLAG_ENV: &str = "QONTINUI_STOP_HOOK_CONTINUATION";

/// Env override for the per-session-per-hour continuation cap.
pub const CAP_ENV: &str = "QONTINUI_STOP_HOOK_CONTINUATION_CAP";

/// Dev/E2E status-source override (see module docs). `absent` ⇒ reachable
/// with no status; any other non-empty value ⇒ that status verbatim.
pub const STATUS_OVERRIDE_ENV: &str = "QONTINUI_STOP_HOOK_STATUS_OVERRIDE";

/// Default hook-driven continuations allowed per session per rolling hour.
pub const DEFAULT_HOURLY_CAP: usize = 5;

/// Rolling window for the continuation cap.
const CAP_WINDOW: Duration = Duration::from_secs(3600);

/// TTL for the process-global coord caches (status + rules). Mirrors
/// `fleet_policy_poller::POLL_INTERVAL`'s 45s freshness posture.
const CACHE_TTL: Duration = Duration::from_secs(45);

/// Built-in umbrella continuation prompt — the fallback when coord's
/// continuation-rules document is unreachable/absent. Coord's
/// `continuation_rules`/`session-continuation` document seeds equivalent
/// wording, so the fallback and the served default read the same.
pub const DEFAULT_CONTINUATION_RULES: &str = "Are there follow-ups? If so work on them; \
     if nothing is left, clean up worktrees/branches and mark the session finished \
     (report status via coord_report_status).";

// ===========================================================================
// Mode (pure)
// ===========================================================================

/// The tri-state continuation mode, parsed from [`FLAG_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Feature dark (default): the endpoint answers `allow` with zero coord
    /// traffic. Unknown flag values also read as `Off` (fail-safe).
    Off,
    /// Compute + log the would-be verdict, always answer `allow`.
    Observe,
    /// Enforce: block-eligible stops answer `block` + prompt.
    On,
}

impl Mode {
    /// Parse the flag value. `None`/empty/unknown ⇒ `Off` (fail-safe).
    pub fn from_flag(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("on") => Mode::On,
            Some("observe") => Mode::Observe,
            _ => Mode::Off,
        }
    }

    /// Read the live mode from the process env (the handler's entry).
    pub fn from_env() -> Self {
        Self::from_flag(std::env::var(FLAG_ENV).ok().as_deref())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Observe => "observe",
            Mode::On => "on",
        }
    }
}

/// What [`FLAG_ENV`] actually SAID, before any default is applied.
///
/// [`Mode::from_flag`] folds "unset" into `Off`, which is right for an ordinary
/// session and wrong for a gate continuation: there the feature ships ON and an
/// explicit `off` is the off-switch, so the two readings must stay apart until
/// the caller knows which kind of session it is deciding for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagReading {
    /// Absent or blank — nobody chose a mode.
    Unset,
    /// A value was set; unknown values have already folded to `Off`
    /// (fail-safe), exactly as [`Mode::from_flag`] does.
    Explicit(Mode),
}

impl FlagReading {
    /// Pure parse of the raw flag value.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") => FlagReading::Unset,
            Some(v) => FlagReading::Explicit(Mode::from_flag(Some(v))),
        }
    }

    /// The RUNNER's own process env (the only env this process can read).
    pub fn from_env() -> Self {
        Self::parse(std::env::var(FLAG_ENV).ok().as_deref())
    }

    /// The mode for one session. A gate continuation reads an unset flag as
    /// `On`; everything else reads it as `Off`. An explicit value wins for
    /// both, so `off` disarms gate continuations too.
    pub fn mode_for(self, gate_continuation: bool) -> Mode {
        match self {
            FlagReading::Explicit(mode) => mode,
            FlagReading::Unset if gate_continuation => Mode::On,
            FlagReading::Unset => Mode::Off,
        }
    }
}

/// The mode the verdict route applies to the session in terminal
/// `terminal_id`: [`FlagReading::mode_for`] with `gate_continuation` taken
/// from the runner's own `ContinuationRegistry` (a live entry carrying a
/// `gate_id`). A path id the registry does not hold — an operator pane, an AI
/// task run, a work-unit dispatch, a continuation restored after a runner
/// restart — gets the ordinary default.
pub fn mode_for_terminal(terminal_id: &str) -> Mode {
    mode_for_terminal_with(FlagReading::from_env(), terminal_id, &|tid| {
        crate::agent_runtime::live_gate_continuation_gate_id(tid).is_some()
    })
}

/// [`mode_for_terminal`] with the flag reading and the registry lookup
/// injected (the test surface — process-global env and the process-global
/// continuation registry both race the parallel harness).
pub fn mode_for_terminal_with(
    reading: FlagReading,
    terminal_id: &str,
    is_gate_continuation: &dyn Fn(&str) -> bool,
) -> Mode {
    reading.mode_for(is_gate_continuation(terminal_id))
}

// ===========================================================================
// Status fetch outcome (pure shape; produced by the coord fetcher)
// ===========================================================================

/// Outcome of the coord session-status read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusFetch {
    /// Coord answered authoritatively. `None` = session known-or-absent with
    /// NO recorded `session_status` (block-eligible, per the decision rule).
    Reachable(Option<String>),
    /// Coord could not be consulted (no JWT / transport / auth / decode).
    /// ALWAYS resolves to allow — fail-open is the phase's top invariant.
    Unreachable(String),
}

// ===========================================================================
// The deterministic decision (pure — the unit-test surface)
// ===========================================================================

/// A computed verdict. `decision` is the wire word (`allow`/`block`);
/// `would_block` marks the block-eligible path for `observe` soak metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: &'static str,
    pub would_block: bool,
    pub reason: String,
}

fn allow(reason: impl Into<String>) -> Verdict {
    Verdict {
        decision: "allow",
        would_block: false,
        reason: reason.into(),
    }
}

fn allow_would_block(reason: impl Into<String>) -> Verdict {
    Verdict {
        decision: "allow",
        would_block: true,
        reason: reason.into(),
    }
}

/// The deterministic decision rule (plan Phase 1):
///
/// block-with-prompt ⇔ mode is `On`
///   AND the stop is not already hook-forced (`stop_hook_active`)
///   AND coord answered with status `working` or ABSENT
///   AND the hourly cap is not exhausted.
///
/// Everything else allows, with the reason recorded for the verdict log.
/// Pure: mode/status/cap are injected (env is read only at the handler edge —
/// global-env mutation in tests races the parallel harness).
pub fn decide(
    mode: Mode,
    stop_hook_active: bool,
    status: &StatusFetch,
    cap_exhausted: bool,
) -> Verdict {
    if mode == Mode::Off {
        return allow("flag off");
    }
    if stop_hook_active {
        return allow("stop_hook_active — continuation already hook-forced");
    }
    let status_word = match status {
        StatusFetch::Unreachable(why) => {
            return allow(format!("coord unreachable — fail-open ({why})"));
        }
        StatusFetch::Reachable(s) => s.as_deref().map(|s| s.trim().to_ascii_lowercase()),
    };
    // Block-eligible ⇔ status is `working` or absent. Every OTHER status —
    // the terminal ones (done/finished/waiting_human/blocked) and anything
    // unrecognized (e.g. watcher-derived `stalled`, future vocabulary) —
    // allows: we only ever force continuation on a session that SAYS it is
    // mid-work or never said anything.
    let block_eligible = matches!(status_word.as_deref(), None | Some("working"));
    if !block_eligible {
        return allow(format!(
            "session_status '{}' is not block-eligible",
            status_word.as_deref().unwrap_or("")
        ));
    }
    let status_desc = status_word.as_deref().unwrap_or("absent").to_string();
    if cap_exhausted {
        return allow_would_block(format!(
            "hourly continuation cap reached (status '{status_desc}')"
        ));
    }
    match mode {
        Mode::Observe => allow_would_block(format!(
            "observe mode — would block (status '{status_desc}')"
        )),
        Mode::On => Verdict {
            decision: "block",
            would_block: true,
            reason: format!("session_status '{status_desc}' — continuation enforced"),
        },
        Mode::Off => unreachable!("handled above"),
    }
}

// ===========================================================================
// Hourly cap — registry-style process-global counter
// ===========================================================================

/// Per-session rolling-window continuation counter. Timestamps are pruned to
/// the window on every touch, so the map stays small (sessions × ≤cap).
struct CapRegistry {
    inner: Mutex<HashMap<String, Vec<Instant>>>,
}

impl CapRegistry {
    fn new() -> Self {
        CapRegistry {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Would one more continuation exceed the cap? Prunes expired stamps.
    /// A poisoned lock reads as EXHAUSTED (fail-safe: never block on error).
    fn would_exceed(&self, key: &str, now: Instant, cap: usize) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return true;
        };
        let stamps = g.entry(key.to_string()).or_default();
        stamps.retain(|t| now.duration_since(*t) < CAP_WINDOW);
        stamps.len() >= cap
    }

    /// Record one ENFORCED continuation. Best-effort (poisoned lock ⇒ no-op).
    fn record(&self, key: &str, now: Instant) {
        if let Ok(mut g) = self.inner.lock() {
            g.entry(key.to_string()).or_default().push(now);
        }
    }
}

static CAP_REGISTRY: OnceLock<CapRegistry> = OnceLock::new();

fn cap_registry() -> &'static CapRegistry {
    CAP_REGISTRY.get_or_init(CapRegistry::new)
}

/// The effective hourly cap: [`CAP_ENV`] when it parses to a positive usize,
/// else [`DEFAULT_HOURLY_CAP`].
fn effective_cap() -> usize {
    std::env::var(CAP_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_HOURLY_CAP)
}

// ===========================================================================
// Pure parsers (unit-test surface for the wire shapes)
// ===========================================================================

/// Extract `stop_hook_active` from the Claude Stop-hook input payload.
/// Absent / non-bool / non-object all read as `false`.
pub fn stop_hook_active_from(input: &Value) -> bool {
    input
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Extract the Claude `session_id` from the Stop-hook input payload.
pub fn session_id_from(input: &Value) -> Option<String> {
    input
        .get("session_id")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Map one work-status door answer onto the verdict's [`StatusFetch`]. Pure.
///
/// Only a ROW is an answer about this session: its status, or `None` when its
/// work axis is unset (ABSENT, block-eligible). coord's `unknown` bucket means
/// "no `claude_code_session_id` row for this id in the acting tenant", which is
/// not "this session has no status": an AI task run's row is keyed by its
/// `task_run_id`, and a session homed in another tenant answers `unknown` too.
/// So `NoRow` is `Unreachable` like every other unsettled read, and allows
/// (fail-open) — never a default rendered as "absent" (served
/// `verification-and-evidence` `unknown-must-not-render-as-a-default`). That
/// second shape is why the read presents the SESSION's tenant's credential
/// ([`crate::mcp::session_work_status::fetch_one_for`]): under the device
/// default, every session homed elsewhere answered `unknown`.
pub fn status_fetch_from_one(one: crate::mcp::session_work_status::OneStatus) -> StatusFetch {
    use crate::mcp::session_work_status::OneStatus;
    match one {
        OneStatus::Row(status) => StatusFetch::Reachable(status),
        OneStatus::NoRow => StatusFetch::Unreachable(
            "coord has no row keyed by this harness session id in the acting tenant".to_string(),
        ),
        OneStatus::Unresolved(why) => StatusFetch::Unreachable(why),
    }
}

/// The continuation-rules document a block prompt is taken from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesDoc {
    /// The prompt text.
    pub text: String,
    /// The row's `current_version` — which revision of the tenant's document
    /// the session was told. `None` for the built-in default, and for a row
    /// that carries none.
    pub version: Option<i64>,
}

impl RulesDoc {
    /// [`DEFAULT_CONTINUATION_RULES`], versionless.
    fn built_in_default() -> Self {
        RulesDoc {
            text: DEFAULT_CONTINUATION_RULES.to_string(),
            version: None,
        }
    }
}

/// Pull the rules text out of a policy-document response — the body-only
/// reader `trust_gate` and `policy_context` share. See [`rules_doc_from_body`]
/// for the shapes accepted.
pub fn rules_from_doc_body(body: &Value) -> Option<String> {
    rules_doc_from_body(body).map(|doc| doc.text)
}

/// Pull the rules text AND version out of a policy-document response. Accepts
/// the document object directly (`{"handle":…,"body":…}`) or wrapped
/// (`{"document":{…}}`). Empty/whitespace bodies read as absent.
pub fn rules_doc_from_body(body: &Value) -> Option<RulesDoc> {
    let doc = if body.get("document").is_some() {
        body.get("document")?
    } else {
        body
    };
    let text = doc
        .get("body")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    Some(RulesDoc {
        text,
        version: doc.get("current_version").and_then(Value::as_i64),
    })
}

// ===========================================================================
// Process-global TTL caches (fleet_policy_poller posture — NOT ETag)
// ===========================================================================

/// One status-cache slot: the coord base, the tenant whose credential read the
/// work axis ([`PolicyTarget::tenant`]) and the harness session id it was read
/// for. A reading under one tenant, or from one coord, is never served for a
/// read under another.
type StatusKey = (String, Option<Uuid>, String);

struct StatusCache {
    inner: Mutex<HashMap<StatusKey, (Instant, StatusFetch)>>,
}

static STATUS_CACHE: OnceLock<StatusCache> = OnceLock::new();

fn status_cache() -> &'static StatusCache {
    STATUS_CACHE.get_or_init(|| StatusCache {
        inner: Mutex::new(HashMap::new()),
    })
}

/// One rules-cache slot: the coord base and the tenant whose credential fetched
/// the document ([`PolicyTarget::tenant`]).
///
/// It used to be ONE slot keyed by nothing, so whichever tenant's rules were
/// fetched last were served to every session on the box for the TTL. Keyed per
/// `(base, tenant)` — the same key `policy_context`'s cache uses — two sessions
/// of two tenants each get their own tenant's document.
type RulesKey = (String, Option<Uuid>);

static RULES_CACHE: OnceLock<Mutex<HashMap<RulesKey, (Instant, RulesDoc)>>> = OnceLock::new();

fn rules_cache() -> &'static Mutex<HashMap<RulesKey, (Instant, RulesDoc)>> {
    RULES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The rules cached for `(base, tenant)`, while within [`CACHE_TTL`] of `now`.
fn rules_cache_lookup(base: &str, tenant: Option<Uuid>, now: Instant) -> Option<RulesDoc> {
    let g = rules_cache().lock().ok()?;
    let (at, doc) = g.get(&(base.to_string(), tenant))?;
    (now.duration_since(*at) < CACHE_TTL).then(|| doc.clone())
}

/// Cache `doc` as `(base, tenant)`'s rules, fetched at `at`.
fn rules_cache_store(base: &str, tenant: Option<Uuid>, at: Instant, doc: RulesDoc) {
    if let Ok(mut g) = rules_cache().lock() {
        g.insert((base.to_string(), tenant), (at, doc));
    }
}

// ===========================================================================
// Coord fetchers (every failure ⇒ Unreachable / default — fail-open)
// ===========================================================================

/// The resolved coord base URL, without a trailing `/`.
pub(crate) fn coord_base() -> String {
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    base.trim_end_matches('/').to_string()
}

/// The DEVICE's default JWT + coord base, the exact accessor pair
/// `fleet_policy_poller` uses. `Err` = unpaired.
///
/// NOT for a session-scoped read or write: on a multi-bound device the
/// default credential names the default tenant, which need not be the
/// session's. Every Stop-hook reader in this module and in
/// [`crate::mcp::session_compliance`] presents the session's tenant instead.
/// The callers that remain are device-scoped by construction:
/// `policy_context` (the coord base and the "is this runner paired at all"
/// gate, asked before any session is read; its fetch credential is the
/// session's), `session_work_status`'s bulk census (`/restart-readiness`, the
/// wind-down observer, the finish heal, transcript autobind) and the
/// continuation-finish relay's device tick.
pub(crate) fn coord_client_parts() -> Result<(String, String), String> {
    let jwt = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "no device JWT (unpaired)".to_string())?;
    Ok((coord_base(), jwt))
}

/// Short-timeout HTTP client for turn-end coord reads. Shared with
/// [`crate::mcp::session_compliance`] — a turn-end hook must never hang on a
/// slow coord.
pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| format!("client build: {e}"))
}

/// The dev/E2E status override ([`STATUS_OVERRIDE_ENV`]), when set — a
/// deterministic local status source that touches no coord and no tenant,
/// logged loudly on every use.
fn status_override(status_key: &str) -> Option<StatusFetch> {
    let ov = std::env::var(STATUS_OVERRIDE_ENV).ok()?;
    let ov = ov.trim().to_string();
    if ov.is_empty() {
        return None;
    }
    warn!(
        session = %status_key,
        override_value = %ov,
        "continuation-verdict: {STATUS_OVERRIDE_ENV} is SET — status source overridden (dev/E2E lever)"
    );
    Some(if ov.eq_ignore_ascii_case("absent") {
        StatusFetch::Reachable(None)
    } else {
        StatusFetch::Reachable(Some(ov))
    })
}

/// Whether `status` is a reading [`decide`] may block on (`working` or absent).
fn is_block_eligible(status: &StatusFetch) -> bool {
    match status {
        StatusFetch::Reachable(None) => true,
        StatusFetch::Reachable(Some(s)) => s.trim().eq_ignore_ascii_case("working"),
        StatusFetch::Unreachable(_) => false,
    }
}

/// The session's work axis, read under the Stop's tenant and TTL-cached per
/// `(tenant, harness session id)`.
///
/// The read is the device-authed work-axis door (see module docs for why not
/// the resolver card), through
/// [`crate::mcp::session_work_status::fetch_one_for`] — the same door, decode
/// path and timeouts `/restart-readiness` uses, under the SESSION's tenant's
/// credential. An UNKNOWN tenant reads nothing and caches nothing: the status is
/// UNKNOWN (`Unreachable`, so the stop is allowed), never a reading taken under
/// the default tenant.
async fn fetch_session_status(
    status_key: &str,
    tenant: &Result<PolicyTarget, String>,
) -> StatusFetch {
    if let Some(over) = status_override(status_key) {
        return over;
    }
    let target = match tenant {
        Ok(target) => *target,
        Err(reason) => {
            return StatusFetch::Unreachable(format!(
                "the session's tenant is unknown, so no tenant's work axis was read ({reason})"
            ))
        }
    };
    let key: StatusKey = (coord_base(), target.tenant, status_key.to_string());
    let now = Instant::now();
    if let Ok(g) = status_cache().inner.lock() {
        if let Some((at, cached)) = g.get(&key) {
            if now.duration_since(*at) < CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let fresh = match tokio::time::timeout(
        STATUS_READ_BUDGET,
        crate::mcp::session_work_status::fetch_one_for(status_key, target),
    )
    .await
    {
        Ok(one) => status_fetch_from_one(one),
        Err(_) => StatusFetch::Unreachable(format!(
            "the work-status read did not answer within the Stop path's {} ms budget",
            STATUS_READ_BUDGET.as_millis()
        )),
    };
    if let Ok(mut g) = status_cache().inner.lock() {
        // Opportunistic prune so the map can't grow unboundedly across many
        // short-lived sessions.
        g.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL * 4);
        // A block-eligible reading is NOT cached: a session blocked at
        // `absent`/`working` that then records `finished` must be allowed on
        // its very next stop, not re-blocked off a 45 s-old answer.
        if is_block_eligible(&fresh) {
            g.remove(&key);
        } else {
            g.insert(key, (now, fresh.clone()));
        }
    }
    fresh
}

/// The continuation rules for the Stop's tenant, TTL-cached per
/// `(base, tenant)`; NEVER fails (falls back to the built-in default umbrella
/// prompt). An UNKNOWN tenant reads no tenant's document and touches no cache
/// entry: it gets the built-in default, versionless.
async fn fetch_continuation_rules(tenant: &Result<PolicyTarget, String>) -> RulesDoc {
    let target = match tenant {
        Ok(target) => *target,
        Err(reason) => {
            debug!(
                reason = %reason,
                "continuation-verdict: session tenant unknown — no tenant's rules were read; \
                 using the built-in default"
            );
            return RulesDoc::built_in_default();
        }
    };
    let base = coord_base();
    let now = Instant::now();
    if let Some(cached) = rules_cache_lookup(&base, target.tenant, now) {
        return cached;
    }
    match fetch_continuation_rules_uncached(&base, target).await {
        Some(doc) => {
            rules_cache_store(&base, target.tenant, now, doc.clone());
            doc
        }
        // Do NOT cache the fallback: a transient coord blip should not pin
        // the default for a TTL when the real document is one retry away.
        None => RulesDoc::built_in_default(),
    }
}

/// Coord's `prompt_documents` coordinates for the umbrella prompt: the
/// document kind and name seeded by coord's `DEFAULT_PROMPT_DOCUMENTS`.
/// Addressed by `(kind, name)` — coord scopes the row to the caller's tenant
/// from the bearer token, so the runner never passes a tenant here.
const RULES_DOC_KIND: &str = "continuation_rules";
const RULES_DOC_NAME: &str = "session-continuation";

/// The coord URL serving the tenant's continuation-rules document.
///
/// `GET` returns the document row with its current `body` — the operator's
/// edits, or coord's seeded default when never edited.
///
/// ## Why `agent-prompt-documents`, not `prompt-documents`
///
/// Coord serves this same row on two doors. `/coord/prompt-documents/*` is the
/// OPERATOR door: it resolves tenancy through coord's `TenantId` extractor,
/// which resolves solely from a verified Cognito operator context and 403s
/// everything else — including this fetch's device JWT (confirmed against
/// prod). Because the fetch fail-opens, pointing here did not break anything
/// loudly; it silently ignored every operator edit and always used
/// [`DEFAULT_CONTINUATION_RULES`], which defeats the whole point of an
/// operator-editable steering document.
///
/// `/coord/agent-prompt-documents/*` is the device/agent door onto the same
/// rows (coord's `require_jwt` sub-router, tenant lifted server-side from the
/// JWT). That is the correct auth layer for a daemon with no operator in the
/// loop — the same split as `/coord/session-messages/*` and
/// `/coord/agent-desired-state`.
fn continuation_rules_url(base: &str) -> String {
    format!("{base}/coord/agent-prompt-documents/{RULES_DOC_KIND}/{RULES_DOC_NAME}")
}

/// The rules GET under `target`'s credential (`coord_http::coord_get_for`
/// with `target.scope`), bounded by [`RULES_FETCH_TIMEOUT`], refused before it
/// can be sent unless its bearer claims `target.tenant`
/// ([`crate::mcp::policy_tenant::build_checked`]).
fn build_rules_get(
    client: &reqwest::Client,
    base: &str,
    target: PolicyTarget,
) -> Result<reqwest::Request, String> {
    policy_tenant::build_checked(
        crate::coord_http::coord_get_for(client, continuation_rules_url(base), target.scope)
            .timeout(RULES_FETCH_TIMEOUT),
        target,
    )
    .map(|(request, _presented)| request)
}

/// The Stop path's bound on the work-status read as a whole — credential
/// resolution (`session_work_status::CREDENTIAL_TIMEOUT`) and the GET
/// (`session_work_status::FETCH_TIMEOUT`) together. A read that has not
/// answered is UNKNOWN, which allows.
const STATUS_READ_BUDGET: Duration = Duration::from_secs(3);

/// The Stop path's bound on the continuation-rules GET, tighter than the
/// shared [`http_client`]'s 4 s: it runs last, after everything else the hook
/// waits for. A slow fetch falls back to the built-in default.
const RULES_FETCH_TIMEOUT: Duration = Duration::from_secs(2);

async fn fetch_continuation_rules_uncached(base: &str, target: PolicyTarget) -> Option<RulesDoc> {
    let client = http_client().ok()?;
    let request = match build_rules_get(&client, base, target) {
        Ok(request) => request,
        Err(reason) => {
            debug!(
                reason = %reason,
                "continuation-verdict: rules fetch not sent — using built-in default"
            );
            return None;
        }
    };
    let resp = client.execute(request).await.ok()?;
    if !resp.status().is_success() {
        debug!(
            status = resp.status().as_u16(),
            "continuation-verdict: rules fetch non-2xx — using built-in default"
        );
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    rules_doc_from_body(&body)
}

// ===========================================================================
// Endpoint entry — compose fetch + guards + decision, log the verdict
// ===========================================================================

/// Wire response for `POST /sessions/{id}/continuation-verdict`. The hook
/// script reads ONLY `decision` + `prompt`; the rest is observability for
/// logs/E2E (`observe` soak reads `would_block`).
#[derive(Debug, Clone, Serialize)]
pub struct VerdictResponse {
    pub decision: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub mode: &'static str,
    pub reason: String,
    pub would_block: bool,
    pub session: String,
    /// The status word the decision saw (`absent` when coord had none,
    /// `unreachable` when coord could not be consulted).
    pub status_seen: String,
    /// Which feature produced this verdict: `continuation` (the
    /// [`FLAG_ENV`]-gated session-autonomy arm) or `compliance` (the
    /// coord-config-gated POLICY_COMPLIANCE nudge). Explicit because the two
    /// are switched independently, so `{"mode":"off","decision":"block"}` is a
    /// legitimate response and anything asserting "flag off ⇒ never blocks"
    /// must discriminate on THIS field, not on `mode`.
    pub source: &'static str,
    /// The tenant every Stop-hook read behind this verdict acted under — the
    /// work-status read, the rules fetch and the compliance config/POST: a
    /// uuid, `default-slot` when the default credential claims no tenant, or
    /// `unknown` when the session's tenant could not be decided and nothing
    /// was read ([`policy_tenant::target_label`]).
    pub tenant: String,
    /// Why [`Self::tenant`] is `unknown` or `default-slot`; `None` for a uuid.
    pub tenant_reason: Option<String>,
    /// The continuation-rules document's `current_version` the prompt was
    /// taken from. `None` when no rules were fetched (the verdict did not
    /// block on continuation), when the built-in default was used (coord
    /// unreadable, or the tenant unknown), or when the row carries none.
    pub rules_version: Option<i64>,
}

/// How a document version reads on a log line — the rules version here, and
/// the compliance config's prompt-document version in `session_compliance`.
pub(crate) fn version_label(version: Option<i64>) -> String {
    version.map_or_else(|| "none".to_string(), |v| v.to_string())
}

/// `(tenant, reason)` as [`policy_tenant::target_label`] renders them.
type TenantLabel = (String, Option<String>);

/// Deliver a session-compliance nudge as a `block` verdict, subject to the
/// loop guards this module already owns.
///
/// The compliance check (plan `2026-07-30-session-compliance-report-
/// enforcement.md` §A4) deliberately does NOT rebuild the guards: it inherits
/// the rolling hourly cap here, and the `stop_hook_active` short-circuit
/// inside [`crate::mcp::session_compliance::observe_turn_end`]. The
/// per-session counter is incremented only on actual DELIVERY — a candidate
/// the cap swallowed stays eligible for a later turn.
///
/// `None` = do not nudge; the caller falls through to its own verdict.
fn deliver_compliance_nudge(
    session_key: &str,
    nudge: Option<crate::mcp::session_compliance::ComplianceNudge>,
    mode: Mode,
    tenant: &TenantLabel,
) -> Option<VerdictResponse> {
    let nudge = nudge?;
    let now = Instant::now();
    // Compliance gets its OWN key in the shared registry rather than sharing
    // the continuation arm's. Under the old hard-coded once-per-session cap it
    // could consume at most 1 of the 5 hourly slots; with a configurable cap
    // it could consume all 5 and silently starve session-continuation for an
    // hour. The two also count different key spaces — the hourly cap keys on
    // the runner session_key, `mark_nudged` on the Claude session id, and one
    // runner session can span several — so sharing a budget was never
    // measuring one thing.
    let cap_key = format!("compliance:{session_key}");
    if cap_registry().would_exceed(&cap_key, now, effective_cap()) {
        info!(
            session = %session_key,
            "continuation-verdict: session-compliance nudge suppressed — hourly cap reached"
        );
        return None;
    }
    // Compare-and-set: two Stops racing for one session must not both deliver.
    // Losing the claim means another turn already sent it — allow this stop.
    //
    // The claim is per `(session, class)`. Before that, the marker was keyed on
    // the session alone, which made the classes mutually exclusive: the first
    // one to fire permanently suppressed the other, so a session that got a
    // report nudge could never be told it skipped the policy pull.
    if !crate::mcp::session_compliance::mark_nudged(
        &nudge.session_id,
        nudge.class,
        nudge.max_attempts,
    ) {
        return None;
    }
    cap_registry().record(&cap_key, now);
    // Names the class AND the attempt, because the string is operator-visible in
    // both the verdict log and `VerdictResponse.reason` — "nudging once" on
    // attempt 3 of 3 is exactly the kind of confidently-wrong line this whole
    // feature exists to stop producing, and an unnamed class makes two
    // independent budgets read as one.
    let attempt =
        crate::mcp::session_compliance::nudge_attempts_reported(&nudge.session_id, nudge.class);
    let reason = format!(
        "session-compliance: verdict `unverified` with a non-empty coord footprint \
         — {} nudge {attempt} of {}",
        nudge.class.as_str(),
        nudge.max_attempts
    );
    // The same one-line-per-Stop verdict log the continuation arm writes, so
    // the soak sample is not blind on exactly the turns that blocked.
    info!(
        session = %session_key,
        mode = mode.as_str(),
        status = "compliance",
        decision = "block",
        would_block = true,
        stop_hook_active = false,
        claude_session = %nudge.session_id,
        reason = %reason,
        tenant = %tenant.0,
        tenant_reason = tenant.1.as_deref().unwrap_or_default(),
        rules_version = "none",
        "continuation-verdict"
    );
    Some(VerdictResponse {
        decision: "block",
        prompt: Some(nudge.prompt),
        mode: mode.as_str(),
        reason,
        would_block: true,
        session: session_key.to_string(),
        status_seen: "compliance".to_string(),
        source: "compliance",
        tenant: tenant.0.clone(),
        tenant_reason: tenant.1.clone(),
        rules_version: None,
    })
}

/// The Stop's ONE tenant decision (Design §1/§4), handed to every Stop-hook
/// reader: the session behind the route — terminal `route_id` (its cwd, else
/// the payload's `cwd`; its coord row, else `session_key`) — through
/// [`policy_tenant::route_policy_target`] under
/// [`policy_tenant::STOP_TENANT_BUDGET`]. `Err` is the UNKNOWN cause: no reader
/// makes a request for it.
///
/// An ANSWERED decision is reused for [`CACHE_TTL`] per `(route id, harness
/// session id)` ([`TenantCacheKey`]): a session's tenant does not move between
/// its turns, and the decision costs the proxy's read plus secure-storage
/// reads on every Stop otherwise. An UNKNOWN decision is never cached, so the
/// next Stop asks again.
async fn stop_tenant(
    route_id: &str,
    session_key: &str,
    hook_input: &Value,
) -> Result<PolicyTarget, String> {
    let key: TenantCacheKey = (route_id.to_string(), session_id_from(hook_input));
    let now = Instant::now();
    if let Some(target) = tenant_cache_lookup(&key, now) {
        return Ok(target);
    }
    let payload_cwd = hook_input.get("cwd").and_then(Value::as_str);
    let decided = policy_tenant::route_policy_target(
        route_id,
        session_key,
        payload_cwd,
        policy_tenant::STOP_TENANT_BUDGET,
    )
    .await;
    tenant_cache_remember(key, now, &decided);
    decided
}

/// One tenant-decision cache slot: the route's path id (the terminal) and the
/// harness session id the Stop payload names — a pane hosts sessions in
/// sequence, so the terminal alone would hand one session's tenant to the next.
type TenantCacheKey = (String, Option<String>);

static TENANT_CACHE: OnceLock<Mutex<HashMap<TenantCacheKey, (Instant, PolicyTarget)>>> =
    OnceLock::new();

fn tenant_cache() -> &'static Mutex<HashMap<TenantCacheKey, (Instant, PolicyTarget)>> {
    TENANT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The answered decision cached for `key`, while within [`CACHE_TTL`] of `now`.
fn tenant_cache_lookup(key: &TenantCacheKey, now: Instant) -> Option<PolicyTarget> {
    let g = tenant_cache().lock().ok()?;
    let (at, target) = g.get(key)?;
    (now.duration_since(*at) < CACHE_TTL).then_some(*target)
}

/// Cache `decided` for `key` when it is an answer; an UNKNOWN is never cached.
fn tenant_cache_remember(key: TenantCacheKey, at: Instant, decided: &Result<PolicyTarget, String>) {
    let Ok(target) = decided else {
        return;
    };
    if let Ok(mut g) = tenant_cache().lock() {
        // Opportunistic prune, as the status cache does.
        g.retain(|_, (stamped, _)| at.duration_since(*stamped) < CACHE_TTL * 4);
        g.insert(key, (at, *target));
    }
}

/// Compute the continuation verdict for `session_key` given the raw Stop-hook
/// input payload, under `mode` (the route resolves it with
/// [`mode_for_terminal`]). `route_id` is the route's path id (the runner
/// TERMINAL id the hook sends) — with `session_key`, what names the session
/// whose tenant every read below acts under. Fail-open end to end; never
/// panics the caller.
pub async fn continuation_verdict(
    route_id: &str,
    session_key: &str,
    hook_input: &Value,
    mode: Mode,
) -> VerdictResponse {
    // The session's tenant, decided ONCE for this Stop and handed to every
    // reader: the compliance config read and POST, the work-status read and
    // the rules fetch. Never the device default on a multi-bound device.
    let tenant = stop_tenant(route_id, session_key, hook_input).await;
    let tenant_label = policy_tenant::target_label(&tenant);

    // Session compliance-report detection + verdict emit (plan
    // `2026-07-30-session-compliance-report-enforcement.md` §A1). Runs on
    // EVERY Stop, independent of [`FLAG_ENV`]: that flag governs the
    // CONTINUATION feature, whereas compliance enforcement is switched by
    // coord's per-tenant config and nothing else (there is deliberately no
    // second local flag). Fail-open — every error path yields `None`, and the
    // whole call is inert until coord serves the config route.
    let compliance = crate::mcp::session_compliance::observe_turn_end(hook_input, &tenant).await;

    // Fast path: dark flag ⇒ allow with ZERO continuation coord traffic (the
    // hook is provisioned fleet-wide, so this is the hot path until the ramp).
    if mode == Mode::Off {
        if let Some(v) = deliver_compliance_nudge(session_key, compliance, mode, &tenant_label) {
            return v;
        }
        debug!(
            session = %session_key,
            tenant = %tenant_label.0,
            tenant_reason = tenant_label.1.as_deref().unwrap_or_default(),
            "continuation-verdict: flag off — allow"
        );
        return VerdictResponse {
            decision: "allow",
            prompt: None,
            mode: mode.as_str(),
            reason: "flag off".to_string(),
            would_block: false,
            session: session_key.to_string(),
            status_seen: "unconsulted".to_string(),
            source: "continuation",
            tenant: tenant_label.0,
            tenant_reason: tenant_label.1,
            rules_version: None,
        };
    }

    let stop_hook_active = stop_hook_active_from(hook_input);
    let status = if stop_hook_active {
        // The guard decides alone — don't spend a coord round-trip.
        StatusFetch::Unreachable("unconsulted (stop_hook_active)".to_string())
    } else {
        // The work-axis door is keyed by the HARNESS session id, which the
        // Stop payload carries. `session_key` may instead be a coord session
        // id (an AI task run the registrar maps), which that door does not key
        // on — so a payload without one is UNKNOWN, never a guessed key.
        match session_id_from(hook_input) {
            Some(status_key) => fetch_session_status(&status_key, &tenant).await,
            None => StatusFetch::Unreachable(
                "the Stop payload carries no session_id to read the work axis by".to_string(),
            ),
        }
    };

    let now = Instant::now();
    let cap = effective_cap();
    let cap_exhausted = cap_registry().would_exceed(session_key, now, cap);

    let verdict = decide(mode, stop_hook_active, &status, cap_exhausted);

    // A continuation block already carries its own prompt — never stack a
    // second one onto the same turn. The compliance nudge is offered only
    // when the continuation arm allows the stop.
    if verdict.decision != "block" {
        if let Some(v) = deliver_compliance_nudge(session_key, compliance, mode, &tenant_label) {
            return v;
        }
    }

    let status_seen = match &status {
        StatusFetch::Reachable(Some(s)) => s.clone(),
        StatusFetch::Reachable(None) => "absent".to_string(),
        StatusFetch::Unreachable(_) => "unreachable".to_string(),
    };

    let (prompt, rules_version) = if verdict.decision == "block" {
        cap_registry().record(session_key, now);
        let rules = fetch_continuation_rules(&tenant).await;
        (Some(rules.text), rules.version)
    } else {
        (None, None)
    };

    // The verdict log — one line per Stop, the observe-soak read surface.
    info!(
        session = %session_key,
        mode = mode.as_str(),
        status = %status_seen,
        decision = verdict.decision,
        would_block = verdict.would_block,
        stop_hook_active,
        reason = %verdict.reason,
        tenant = %tenant_label.0,
        tenant_reason = tenant_label.1.as_deref().unwrap_or_default(),
        rules_version = %version_label(rules_version),
        "continuation-verdict"
    );

    VerdictResponse {
        decision: verdict.decision,
        prompt,
        mode: mode.as_str(),
        reason: verdict.reason,
        would_block: verdict.would_block,
        session: session_key.to_string(),
        status_seen,
        source: "continuation",
        tenant: tenant_label.0,
        tenant_reason: tenant_label.1,
        rules_version,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests;
