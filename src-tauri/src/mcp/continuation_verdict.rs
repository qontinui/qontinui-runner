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
//! ## Coord fetches + cache
//!
//! Coord reads are cached in process-global TTL caches (45s, mirroring
//! `fleet_policy_poller`'s poll-into-process-global-cache posture — NOT ETag):
//!
//! - **session status** — `GET {coord}/coord/agent-sessions/{key}` (the
//!   session-identity resolver card) with the device JWT. Today that route is
//!   operator-SSO-gated in prod, so a device-JWT read typically 401s ⇒
//!   `Unreachable` ⇒ allow — the Phase-3/4 coord phases add the fleet-authed
//!   status read; the seam here is already shaped for it (an explicit
//!   `session_status` field on the card is preferred the moment coord ships
//!   one). A `session_not_found` 404 means coord IS reachable and has no row ⇒
//!   status ABSENT (block-eligible), per the phase's decision rule.
//! - **continuation rules** — `GET {coord}/coord/policy-documents/
//!   continuation-rules` (the Phase-2 `prompt_documents` stand-in). Any
//!   failure falls back to [`DEFAULT_CONTINUATION_RULES`].
//!
//! `QONTINUI_STOP_HOOK_STATUS_OVERRIDE` is a dev/E2E lever: when set, the
//! status fetch short-circuits to that value (`absent` ⇒ reachable-but-no-
//! status) without touching coord, so the block path is live-verifiable
//! before the coord read surface exists. It is logged loudly on every use.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info, warn};

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
/// continuation-rules document is unreachable/absent (and the seed text the
/// Phase-2 `continuation_rules` document kind will carry).
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

/// Pull a `session_status` out of the session-identity resolver response
/// (`GET /coord/agent-sessions/:id` ⇒ `{"resolved":[<card>...],"count":N}`).
///
/// Preference order (forward-compatible with the Phase-3/4 coord read):
/// 1. an explicit `session_status` on the card,
/// 2. an explicit `working_on.session.session_status`,
/// 3. the card's liveness `status`: `closed` ⇒ treat as `finished` (a closed
///    session is never continued); `live`/`stale` ⇒ NO status recorded ⇒
///    `None` (absent, block-eligible).
///
/// Zero cards ⇒ `None` (coord reachable, session unknown ⇒ absent).
pub fn status_from_resolve_body(body: &Value) -> Option<String> {
    let card = body.get("resolved").and_then(|r| r.get(0))?;
    let explicit = card
        .get("session_status")
        .and_then(Value::as_str)
        .or_else(|| {
            card.get("working_on")
                .and_then(|w| w.get("session"))
                .and_then(|s| s.get("session_status"))
                .and_then(Value::as_str)
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if explicit.is_some() {
        return explicit;
    }
    match card.get("status").and_then(Value::as_str) {
        Some("closed") => Some("finished".to_string()),
        _ => None,
    }
}

/// Pull the rules text out of a policy-document response. Accepts the
/// document object directly (`{"handle":…,"body":…}`) or wrapped
/// (`{"document":{…}}`). Empty/whitespace bodies read as absent.
pub fn rules_from_doc_body(body: &Value) -> Option<String> {
    let doc = if body.get("document").is_some() {
        body.get("document")?
    } else {
        body
    };
    doc.get("body")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ===========================================================================
// Process-global TTL caches (fleet_policy_poller posture — NOT ETag)
// ===========================================================================

struct StatusCache {
    inner: Mutex<HashMap<String, (Instant, StatusFetch)>>,
}

static STATUS_CACHE: OnceLock<StatusCache> = OnceLock::new();

fn status_cache() -> &'static StatusCache {
    STATUS_CACHE.get_or_init(|| StatusCache {
        inner: Mutex::new(HashMap::new()),
    })
}

static RULES_CACHE: OnceLock<Mutex<Option<(Instant, String)>>> = OnceLock::new();

fn rules_cache() -> &'static Mutex<Option<(Instant, String)>> {
    RULES_CACHE.get_or_init(|| Mutex::new(None))
}

// ===========================================================================
// Coord fetchers (every failure ⇒ Unreachable / default — fail-open)
// ===========================================================================

/// Device JWT + coord base, the exact accessor pair `fleet_policy_poller`
/// uses. `Err` = cannot consult coord (⇒ Unreachable ⇒ allow).
fn coord_client_parts() -> Result<(String, String), String> {
    let jwt = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "no device JWT (unpaired)".to_string())?;
    let base = crate::mcp::agent_worktrees::coord_http_base()
        .map_err(|e| format!("coord base unresolved: {e}"))?;
    Ok((base.trim_end_matches('/').to_string(), jwt))
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| format!("client build: {e}"))
}

/// Fetch (uncached) the session's coord status. See module docs for the
/// route choice + the honest posture on today's auth gating.
async fn fetch_session_status_uncached(session_key: &str) -> StatusFetch {
    // Dev/E2E override — deterministic local status source, logged loudly.
    if let Ok(ov) = std::env::var(STATUS_OVERRIDE_ENV) {
        let ov = ov.trim().to_string();
        if !ov.is_empty() {
            warn!(
                session = %session_key,
                override_value = %ov,
                "continuation-verdict: {STATUS_OVERRIDE_ENV} is SET — status source overridden (dev/E2E lever)"
            );
            return if ov.eq_ignore_ascii_case("absent") {
                StatusFetch::Reachable(None)
            } else {
                StatusFetch::Reachable(Some(ov))
            };
        }
    }

    let (base, jwt) = match coord_client_parts() {
        Ok(p) => p,
        Err(e) => return StatusFetch::Unreachable(e),
    };
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return StatusFetch::Unreachable(e),
    };
    let url = format!("{base}/coord/agent-sessions/{session_key}");
    let resp = match client.get(&url).bearer_auth(&jwt).send().await {
        Ok(r) => r,
        Err(e) => return StatusFetch::Unreachable(format!("request: {e}")),
    };
    let status = resp.status();
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) if status.is_success() => {
            return StatusFetch::Unreachable(format!("undecodable 2xx body from {url}"));
        }
        Err(_) => Value::Null,
    };
    if status.is_success() {
        return StatusFetch::Reachable(status_from_resolve_body(&body));
    }
    // A session_not_found 404 = coord IS reachable and has no row ⇒ status
    // ABSENT (block-eligible). Any OTHER 404 (older coord without the route)
    // and every auth/5xx outcome is Unreachable ⇒ allow.
    if status == reqwest::StatusCode::NOT_FOUND
        && body.get("error").and_then(Value::as_str) == Some("session_not_found")
    {
        return StatusFetch::Reachable(None);
    }
    StatusFetch::Unreachable(format!("coord status {}", status.as_u16()))
}

/// TTL-cached session-status read.
async fn fetch_session_status(session_key: &str) -> StatusFetch {
    let now = Instant::now();
    if let Ok(g) = status_cache().inner.lock() {
        if let Some((at, cached)) = g.get(session_key) {
            if now.duration_since(*at) < CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let fresh = fetch_session_status_uncached(session_key).await;
    if let Ok(mut g) = status_cache().inner.lock() {
        // Opportunistic prune so the map can't grow unboundedly across many
        // short-lived sessions.
        g.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL * 4);
        g.insert(session_key.to_string(), (now, fresh.clone()));
    }
    fresh
}

/// TTL-cached continuation-rules text; NEVER fails (falls back to the
/// built-in default umbrella prompt).
async fn fetch_continuation_rules() -> String {
    let now = Instant::now();
    if let Ok(g) = rules_cache().lock() {
        if let Some((at, cached)) = g.as_ref() {
            if now.duration_since(*at) < CACHE_TTL {
                return cached.clone();
            }
        }
    }
    let fetched = fetch_continuation_rules_uncached().await;
    match fetched {
        Some(text) => {
            if let Ok(mut g) = rules_cache().lock() {
                *g = Some((now, text.clone()));
            }
            text
        }
        // Do NOT cache the fallback: a transient coord blip should not pin
        // the default for a TTL when the real document is one retry away.
        None => DEFAULT_CONTINUATION_RULES.to_string(),
    }
}

async fn fetch_continuation_rules_uncached() -> Option<String> {
    let (base, jwt) = match coord_client_parts() {
        Ok(p) => p,
        Err(e) => {
            debug!("continuation-verdict: rules fetch skipped ({e}) — using built-in default");
            return None;
        }
    };
    let client = http_client().ok()?;
    // Phase-2 stand-in: coord's policy-documents surface. When the
    // `prompt_documents` phases land, this repoints to `kind=continuation_rules`.
    let url = format!("{base}/coord/policy-documents/continuation-rules");
    let resp = client.get(&url).bearer_auth(&jwt).send().await.ok()?;
    if !resp.status().is_success() {
        debug!(
            status = resp.status().as_u16(),
            "continuation-verdict: rules fetch non-2xx — using built-in default"
        );
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    rules_from_doc_body(&body)
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
}

/// Compute the continuation verdict for `session_key` given the raw Stop-hook
/// input payload. Fail-open end to end; never panics the caller.
pub async fn continuation_verdict(session_key: &str, hook_input: &Value) -> VerdictResponse {
    let mode = Mode::from_env();

    // Fast path: dark flag ⇒ allow with ZERO coord traffic (the hook is
    // provisioned fleet-wide, so this is the hot path until the ramp).
    if mode == Mode::Off {
        debug!(
            session = %session_key,
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
        };
    }

    let stop_hook_active = stop_hook_active_from(hook_input);
    let status = if stop_hook_active {
        // The guard decides alone — don't spend a coord round-trip.
        StatusFetch::Unreachable("unconsulted (stop_hook_active)".to_string())
    } else {
        fetch_session_status(session_key).await
    };

    let now = Instant::now();
    let cap = effective_cap();
    let cap_exhausted = cap_registry().would_exceed(session_key, now, cap);

    let verdict = decide(mode, stop_hook_active, &status, cap_exhausted);

    let status_seen = match &status {
        StatusFetch::Reachable(Some(s)) => s.clone(),
        StatusFetch::Reachable(None) => "absent".to_string(),
        StatusFetch::Unreachable(_) => "unreachable".to_string(),
    };

    let prompt = if verdict.decision == "block" {
        cap_registry().record(session_key, now);
        Some(fetch_continuation_rules().await)
    } else {
        None
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
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reachable(s: Option<&str>) -> StatusFetch {
        StatusFetch::Reachable(s.map(String::from))
    }

    // ── Mode parsing ─────────────────────────────────────────────────────

    #[test]
    fn mode_defaults_off_and_unknown_is_off() {
        assert_eq!(Mode::from_flag(None), Mode::Off);
        assert_eq!(Mode::from_flag(Some("")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("bogus")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("OFF")), Mode::Off);
        assert_eq!(Mode::from_flag(Some("observe")), Mode::Observe);
        assert_eq!(Mode::from_flag(Some(" Observe ")), Mode::Observe);
        assert_eq!(Mode::from_flag(Some("on")), Mode::On);
        assert_eq!(Mode::from_flag(Some("ON")), Mode::On);
    }

    // ── decide(): the full status × flag matrix ──────────────────────────

    #[test]
    fn off_allows_everything() {
        for status in [
            reachable(Some("working")),
            reachable(None),
            StatusFetch::Unreachable("x".into()),
        ] {
            let v = decide(Mode::Off, false, &status, false);
            assert_eq!(v.decision, "allow");
            assert!(!v.would_block);
        }
    }

    #[test]
    fn on_blocks_working_and_absent() {
        for status in [reachable(Some("working")), reachable(None)] {
            let v = decide(Mode::On, false, &status, false);
            assert_eq!(v.decision, "block", "status {:?}", status);
            assert!(v.would_block);
        }
        // Case-insensitive + whitespace-tolerant status words.
        let v = decide(Mode::On, false, &reachable(Some(" Working ")), false);
        assert_eq!(v.decision, "block");
    }

    #[test]
    fn on_allows_terminal_statuses() {
        for s in ["done", "finished", "waiting_human", "blocked"] {
            let v = decide(Mode::On, false, &reachable(Some(s)), false);
            assert_eq!(v.decision, "allow", "status {s}");
            assert!(!v.would_block, "status {s}");
        }
    }

    #[test]
    fn on_allows_unrecognized_statuses() {
        // Only working/absent are block-eligible — watcher-derived `stalled`
        // and any future vocabulary must fail toward allow.
        for s in ["stalled", "some_future_word"] {
            let v = decide(Mode::On, false, &reachable(Some(s)), false);
            assert_eq!(v.decision, "allow", "status {s}");
        }
    }

    #[test]
    fn unreachable_fails_open_in_every_mode() {
        for mode in [Mode::Observe, Mode::On] {
            let v = decide(mode, false, &StatusFetch::Unreachable("boom".into()), false);
            assert_eq!(v.decision, "allow");
            assert!(!v.would_block);
            assert!(v.reason.contains("fail-open"), "reason: {}", v.reason);
        }
    }

    #[test]
    fn observe_computes_would_block_but_allows() {
        let v = decide(Mode::Observe, false, &reachable(Some("working")), false);
        assert_eq!(v.decision, "allow");
        assert!(v.would_block);
        let v = decide(Mode::Observe, false, &reachable(None), false);
        assert_eq!(v.decision, "allow");
        assert!(v.would_block);
        // Non-eligible status in observe: allow, NOT would-block.
        let v = decide(Mode::Observe, false, &reachable(Some("finished")), false);
        assert!(!v.would_block);
    }

    #[test]
    fn stop_hook_active_guard_never_reblocks() {
        for mode in [Mode::Observe, Mode::On] {
            let v = decide(mode, true, &reachable(Some("working")), false);
            assert_eq!(v.decision, "allow");
            assert!(!v.would_block);
            assert!(v.reason.contains("stop_hook_active"));
        }
    }

    #[test]
    fn cap_exhaustion_allows_but_records_would_block() {
        let v = decide(Mode::On, false, &reachable(Some("working")), true);
        assert_eq!(v.decision, "allow");
        assert!(v.would_block);
        assert!(v.reason.contains("cap"));
    }

    // ── Hourly cap registry ──────────────────────────────────────────────

    #[test]
    fn cap_registry_trips_at_cap_and_is_per_session() {
        let reg = CapRegistry::new();
        let now = Instant::now();
        let cap = 3;
        for i in 0..cap {
            assert!(
                !reg.would_exceed("s1", now, cap),
                "under cap at continuation {i}"
            );
            reg.record("s1", now);
        }
        assert!(reg.would_exceed("s1", now, cap), "cap reached");
        // A DIFFERENT session is unaffected.
        assert!(!reg.would_exceed("s2", now, cap));
    }

    #[test]
    fn cap_registry_window_expires() {
        let reg = CapRegistry::new();
        let start = Instant::now();
        let cap = 2;
        reg.record("s", start);
        reg.record("s", start);
        assert!(reg.would_exceed("s", start, cap));
        // Just past the rolling window the stamps prune away.
        let later = start + CAP_WINDOW + Duration::from_secs(1);
        assert!(!reg.would_exceed("s", later, cap));
    }

    #[test]
    fn default_cap_is_five() {
        assert_eq!(DEFAULT_HOURLY_CAP, 5);
    }

    // ── Hook-input parsing ───────────────────────────────────────────────

    #[test]
    fn stop_hook_active_parses_leniently() {
        assert!(stop_hook_active_from(&json!({"stop_hook_active": true})));
        assert!(!stop_hook_active_from(&json!({"stop_hook_active": false})));
        assert!(!stop_hook_active_from(&json!({})));
        assert!(!stop_hook_active_from(&json!({"stop_hook_active": "yes"})));
        assert!(!stop_hook_active_from(&Value::Null));
    }

    #[test]
    fn session_id_parses_and_rejects_empty() {
        assert_eq!(
            session_id_from(&json!({"session_id": "abc-123"})),
            Some("abc-123".to_string())
        );
        assert_eq!(session_id_from(&json!({"session_id": "  "})), None);
        assert_eq!(session_id_from(&json!({})), None);
    }

    // ── Wire-shape parsers ───────────────────────────────────────────────

    #[test]
    fn status_prefers_explicit_session_status_fields() {
        // Forward-compatible: an explicit card-level session_status wins.
        let b = json!({"resolved": [{"status": "live", "session_status": "waiting_human"}]});
        assert_eq!(
            status_from_resolve_body(&b),
            Some("waiting_human".to_string())
        );
        // Nested working_on.session.session_status is honored too.
        let b = json!({"resolved": [{
            "status": "live",
            "working_on": {"session": {"session_status": "working"}}
        }]});
        assert_eq!(status_from_resolve_body(&b), Some("working".to_string()));
    }

    #[test]
    fn status_derives_finished_from_closed_card() {
        let b = json!({"resolved": [{"status": "closed"}], "count": 1});
        assert_eq!(status_from_resolve_body(&b), Some("finished".to_string()));
    }

    #[test]
    fn status_absent_for_live_card_without_session_status() {
        let b = json!({"resolved": [{"status": "live"}], "count": 1});
        assert_eq!(status_from_resolve_body(&b), None);
        let b = json!({"resolved": [{"status": "stale"}], "count": 1});
        assert_eq!(status_from_resolve_body(&b), None);
    }

    #[test]
    fn status_absent_for_zero_cards_or_garbage() {
        assert_eq!(status_from_resolve_body(&json!({"resolved": []})), None);
        assert_eq!(status_from_resolve_body(&json!({})), None);
        assert_eq!(status_from_resolve_body(&Value::Null), None);
    }

    #[test]
    fn rules_parse_direct_and_wrapped_shapes() {
        let direct = json!({"handle": "continuation-rules", "body": "Do the follow-ups."});
        assert_eq!(
            rules_from_doc_body(&direct),
            Some("Do the follow-ups.".to_string())
        );
        let wrapped = json!({"document": {"body": "Wrapped text"}});
        assert_eq!(
            rules_from_doc_body(&wrapped),
            Some("Wrapped text".to_string())
        );
        assert_eq!(rules_from_doc_body(&json!({"body": "  "})), None);
        assert_eq!(rules_from_doc_body(&json!({})), None);
    }

    #[test]
    fn default_rules_text_is_the_umbrella_prompt() {
        assert!(DEFAULT_CONTINUATION_RULES.contains("follow-ups"));
        assert!(DEFAULT_CONTINUATION_RULES.contains("finished"));
    }
}
