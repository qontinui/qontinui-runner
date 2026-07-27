//! Coord-mcp `.mcp.json` provisioning — the leaf module owning every helper that
//! writes a coord device/agent JWT into a session's `.mcp.json`.
//!
//! Extracted out of `agent_runtime.rs` so the terminal chokepoint
//! (`agent_worktree::isolated_edit::acquire_for_terminal`) can call provisioning
//! WITHOUT creating a circular dependency: `agent_runtime` already depends on
//! `agent_worktree`, so `agent_worktree → agent_runtime` would close a cycle.
//! This module depends only on `crate::auth`, `std::fs`, `serde_json`, `base64`
//! (plus read-only port/handle lookups) — a leaf both callers can share.
//!
//! # Live-token loopback proxy (plan 2026-06-09-coord-mcp-live-token-proxy)
//!
//! Coord device JWTs have a ~4h TTL and Claude Code's MCP client reads
//! `.mcp.json` exactly once at connect — a static baked bearer dies with the
//! snapshot, and re-stamping the file does nothing (the client never re-reads).
//! DEVICE-provisioned sessions therefore get a config pointing at the runner's
//! own loopback `POST /coord-mcp` route, which injects a freshly-read device
//! JWT per request (see `mcp_api::coord_mcp_proxy_handler`). Authentication to
//! the proxy is a per-session nonce ([`COORD_MCP_PROXY_KEY_HEADER`]) held in an
//! in-memory map that is ALSO mirrored to the encrypted local store
//! (plan 2026-06-13 Phase 3b, gated by `COORD_MCP_PERSIST_NONCES`, default ON)
//! so an already-written `.mcp.json` keeps validating across a runner
//! rebuild/restart — the MCP client never re-reads the file, so a process-only
//! nonce would 401 every live agent after a routine restart.
//!
//! SCOPE-ELEVATION TRAP: agent-spawn sessions deliberately carry a narrower
//! coord-minted `SubType::Agent` JWT. The DEVICE proxy attaches the live DEVICE
//! JWT, so an agent session must NEVER route through the device proxy. Instead,
//! agent sessions get their OWN per-agent proxy ([`ProxyPrincipal::Agent`]) that
//! injects THEIR OWN refreshed agent JWT — held in [`AGENT_TOKENS`], a
//! process-global `SharedToken` slot refreshed proactively from the agent's
//! heartbeat loop so the 4h TTL never expires for a live agent. The proxy gate
//! structurally binds nonce→principal→`sub_type`: a device nonce can only ever
//! forward a `sub_type == "device"` bearer, and an agent nonce a
//! `sub_type == "agent"` bearer — neither can elevate into the other.
//!
//! # Multi-tenant disposition (Phase 8b, plan
//! `2026-07-02-session-scoped-multi-tenant-device-binding` §D4)
//!
//! The proxy is SESSION-SCOPED where it matters and default-scoped where
//! that is the honest binding:
//!
//! - **Agent principal — session-scoped by construction.** A coord-spawned
//!   agent session's bearer is its own coord-minted agent JWT, whose
//!   `tenant_id` claim is the SESSION's binding frozen at mint (coord
//!   Phase 4 validates membership at spawn). Two concurrent agent sessions
//!   for two tenants each present their own tenant's claim with zero
//!   runner-side selection logic.
//! - **Device principal — the SESSION's tenant by construction (B3).** A
//!   device nonce freezes the tenant it was provisioned under
//!   (`machine.json::active_tenant_id` — the same source
//!   `stamp_session_tenant` records on the session's coord row) on its
//!   [`NonceBinding::session_tenant`]. The proxy injects that tenant's
//!   credential via `auth::device_bearer_for(session_tenant)`, NOT the
//!   legacy `access_token` slot: when the session tenant IS the default
//!   binding this serves the legacy slot unchanged, but a NON-default
//!   session presents ITS tenant's device JWT — or, on a slot miss,
//!   degrades to the refresh/401 path and sends nothing (never another
//!   tenant's token). Restored / adopted nonces carry no tenant (`None`)
//!   and fall back to the legacy default slot, the pre-B3 behavior.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tracing::{info, warn};
use uuid::Uuid;

/// Which identity a registered proxy nonce is bound to. The nonce→principal
/// binding is the structural backstop against the scope-elevation trap: the
/// pre-forward gate ([`proxy_request_gate`]) requires the injected bearer's
/// `sub_type` claim to match the principal, so a device nonce can never forward
/// an agent token (or vice-versa).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProxyPrincipal {
    /// The runner's own device identity — the bearer is the live device JWT read
    /// from `AuthManager` per request.
    Device,
    /// A specific spawned agent — the bearer is THAT agent's own refreshed JWT,
    /// looked up from [`AGENT_TOKENS`] by `agent_id` per request.
    Agent { agent_id: Uuid },
}

impl ProxyPrincipal {
    /// The `sub_type` claim value a forwarded bearer must carry for this
    /// principal. The gate rejects any bearer whose `sub_type` differs.
    fn expected_sub_type(&self) -> &'static str {
        match self {
            ProxyPrincipal::Device => "device",
            ProxyPrincipal::Agent { .. } => "agent",
        }
    }
}

/// How long a registered nonce lives, and whether it may be persisted.
///
/// Split introduced by plan
/// `2026-07-17-universal-coord-device-identity-for-any-session` (§1/E). The two
/// classes exist because they answer to OPPOSITE constraints:
///
/// - [`NonceLifetime::Persistent`] — every nonce minted by the trusted in-process
///   spawn code (the identity seam, the terminal chokepoint, the boot self-heal).
///   Today's semantics, UNCHANGED: no expiry, mirrored to the encrypted store
///   when the principal is Device ([`persist_proxy_nonces`]), graced on eviction.
///   Persistence + the grace window exist BECAUSE the MCP client reads its config
///   exactly once at connect (see the module doc): a nonce that dies under a live
///   client 401s it with no way to recover, so these must outlive a restart.
/// - [`NonceLifetime::Ephemeral`] — minted ONLY by the
///   `/coord-mcp/provision-session` route, i.e. on behalf of a session the runner
///   did not spawn and cannot vouch for. Bounded expiry, NEVER persisted, and
///   revoked the moment the machine is opted back out ([`session_identity_gate`]).
///
/// **A short TTL fights the persistence design — so it is scoped, not global.**
/// Shortening every nonce would 401 every live agent across a routine runner
/// rebuild, which is precisely the failure nonce persistence was built to fix.
/// Only the route's own nonces are bounded; the seam's are untouched. That is
/// also why eviction is class-scoped in [`mint_and_register_nonce`]: a bare
/// session minting for a cwd must never evict a live PTY terminal's nonce for
/// that same cwd.
#[derive(Clone, Debug, PartialEq)]
enum NonceLifetime {
    /// Runner-spawn provenance — no expiry, persistable, graced. Today's shape.
    Persistent,
    /// Mint-route provenance — dies at `expires_at`, never reaches disk, and is
    /// revoked live by opting the machine out.
    Ephemeral { expires_at: std::time::Instant },
}

impl NonceLifetime {
    /// A fresh mint-route lifetime: now + [`EPHEMERAL_NONCE_TTL`].
    fn ephemeral() -> Self {
        NonceLifetime::Ephemeral {
            expires_at: std::time::Instant::now() + EPHEMERAL_NONCE_TTL,
        }
    }

    fn is_ephemeral(&self) -> bool {
        matches!(self, NonceLifetime::Ephemeral { .. })
    }
}

/// Bounded lifetime of a mint-route ([`NonceLifetime::Ephemeral`]) nonce.
///
/// **Why 12h and not minutes.** The obvious reading of "short-TTL" — a few
/// minutes — is WRONG here and would ship a broken feature: the MCP client reads
/// its `--mcp-config` once at connect and never re-reads it (module doc), so the
/// nonce must stay valid for the WHOLE session, not just long enough to hand it
/// over. A minutes-scale TTL would silently 401 a bare session mid-turn. 12h
/// bounds a leaked nonce to at most one working day while covering any plausible
/// session.
///
/// What actually contains the exposure is the combination, not the number: an
/// ephemeral nonce (a) expires, (b) NEVER reaches disk — so unlike a seam nonce
/// it cannot be replayed after a runner restart, and (c) stops validating the
/// instant the operator removes the opt-in marker. Compare a Persistent nonce,
/// which is unbounded AND survives restarts by design.
const EPHEMERAL_NONCE_TTL: std::time::Duration = std::time::Duration::from_secs(12 * 60 * 60);

/// What a registered proxy nonce maps to: the session workdir it was provisioned
/// into, the identity ([`ProxyPrincipal`]) whose bearer the proxy may inject for
/// it, and its [`NonceLifetime`] (which decides expiry, persistence, and grace).
#[derive(Clone, Debug)]
struct NonceBinding {
    workdir: String,
    principal: ProxyPrincipal,
    lifetime: NonceLifetime,
    /// The tenant this session was provisioned under, frozen at mint time
    /// (`machine.json::active_tenant_id` — the same value
    /// `stamp_session_tenant` records on the session's coord row). The
    /// DEVICE proxy path selects its injected bearer with
    /// `auth::device_bearer_for(session_tenant.as_ref())` so the proxy acts
    /// as the SESSION's tenant, not whatever binding happens to own the
    /// legacy `access_token` slot (B3). `None` on a single-tenant install
    /// with no active pin (→ `device_bearer_for(None)` = the legacy default
    /// slot, byte-identical to pre-B3 behavior). Unused for Agent nonces —
    /// their bearer is the agent JWT, whose tenant claim is frozen at mint.
    session_tenant: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Session-provisioned coord identity: the TWO gates (plan 2026-07-17 §1/§3)
// ---------------------------------------------------------------------------

/// Master enable flag for session-provisioned coord identity — the
/// `POST /coord-mcp/provision-session` mint route. **Default OFF: the feature
/// ships dark**, exactly like [`crate::install_effects_producer::intercept::shim_materializer::ENABLE_FLAG`]
/// (`QONTINUI_INSTALL_INTERCEPT_ENABLED`), whose constant shape this copies.
/// Flag off ⇒ the route denies every request ⇒ zero behavior change for every
/// existing session, spawned or bare.
pub(crate) const SESSION_IDENTITY_ENABLE_FLAG: &str = "QONTINUI_SESSION_COORD_IDENTITY_ENABLED";

/// File name of the per-machine operator opt-in marker, under `~/.qontinui/`.
/// Its mere existence is the signal; contents are never read.
///
/// Re-exported from the LIB crate ([`qontinui_runner_lib::profile_cli`]) so this
/// authoritative runner-side gate and the standalone `qontinui-shim` `.exe` share
/// ONE source of truth for the marker — a rename can no longer silently desync
/// the two processes.
pub(crate) use qontinui_runner_lib::profile_cli::SESSION_IDENTITY_MARKER_FILE;

/// Absolute path of the opt-in marker (`~/.qontinui/allow-session-coord-identity`).
/// `None` when the home dir is unresolvable — which [`session_identity_gate`]
/// treats as NOT opted in (fail-closed: an unresolvable home must never read as
/// consent). Delegates to the shared lib resolver so the gate and the shim
/// compute the identical path (directory + filename), not merely the filename.
pub(crate) fn session_identity_marker_path() -> Option<std::path::PathBuf> {
    qontinui_runner_lib::profile_cli::session_identity_marker_path()
}

/// Why the mint route refused. Typed rather than a bare bool so the route can
/// return an explicit, actionable reason — the runner's "no silent empty
/// responses" rule. A denied caller must be able to tell "the feature is dark"
/// from "this machine has not opted in", because the fixes are different.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionIdentityDenial {
    /// The master flag is unset/falsy — the feature is dark on this runner.
    FlagOff,
    /// The flag is on but the operator has not dropped the opt-in marker.
    NotOptedIn,
}

impl SessionIdentityDenial {
    /// Machine-readable code for the route's JSON error body.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            SessionIdentityDenial::FlagOff => "COORD_MCP_PROVISION_DISABLED",
            SessionIdentityDenial::NotOptedIn => "COORD_MCP_PROVISION_NOT_OPTED_IN",
        }
    }

    /// Human/agent-actionable explanation — names the exact lever to flip.
    pub(crate) fn message(&self) -> String {
        match self {
            SessionIdentityDenial::FlagOff => format!(
                "session-provisioned coord identity is disabled on this runner — \
                 set {SESSION_IDENTITY_ENABLE_FLAG}=1 in the runner's environment \
                 and restart it to enable"
            ),
            SessionIdentityDenial::NotOptedIn => {
                let path = session_identity_marker_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| format!("~/.qontinui/{SESSION_IDENTITY_MARKER_FILE}"));
                format!(
                    "this machine has not opted in to session-provisioned coord \
                     identity — create the marker file {path} to opt in (delete it \
                     to revoke, which also invalidates already-minted session nonces)"
                )
            }
        }
    }
}

/// Pure resolver for [`session_identity_gate`] — both gates, no I/O, so the
/// default-OFF posture is unit-testable without touching process-global env or
/// the operator's real home dir.
fn resolve_session_identity_gate(
    flag_on: bool,
    marker_exists: bool,
) -> Result<(), SessionIdentityDenial> {
    if !flag_on {
        return Err(SessionIdentityDenial::FlagOff);
    }
    if !marker_exists {
        return Err(SessionIdentityDenial::NotOptedIn);
    }
    Ok(())
}

/// The authorization gate for session-provisioned coord identity: the master
/// flag AND the per-machine opt-in marker. BOTH are required — neither alone
/// grants identity.
///
/// # Why two gates instead of a nonce check
///
/// Every OTHER `/coord-mcp/*` route is nonce-gated, which is a strong gate: a
/// caller must already hold a runner-minted per-session key. The mint route
/// structurally CANNOT be nonce-gated — it is what issues the nonce. So these
/// two gates stand IN PLACE OF that check, and they are the entire authorization
/// story for the route. See [`crate::mcp_api::coord_provision_session_handler`].
///
/// # Why "same machine" is not itself an authorization signal
///
/// On a single-user dev box every process runs as the same OS user, so reaching
/// `127.0.0.1` proves nothing — a compromised dependency's post-install script
/// could mint device identity and act as the operator against coord. The marker
/// converts "any local process" into "any local process on a machine the
/// operator deliberately, revocably opted in", which is a decision the operator
/// made rather than one the network topology made for them.
///
/// # Live, not just mint-time
///
/// This is re-checked on every request that presents an
/// [`NonceLifetime::Ephemeral`] nonce ([`live_binding`]), so deleting the marker
/// REVOKES already-minted session nonces instead of merely blocking new ones. It
/// is the operator's actual off switch. Cheap by construction: only ephemeral
/// bindings pay the check, so a runner-spawned terminal never does.
pub(crate) fn session_identity_gate() -> Result<(), SessionIdentityDenial> {
    // Flag first, short-circuiting: in the default (dark) posture this costs one
    // env read and never touches the filesystem.
    let flag_on = matches!(
        std::env::var(SESSION_IDENTITY_ENABLE_FLAG)
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    );
    if !flag_on {
        return Err(SessionIdentityDenial::FlagOff);
    }
    let marker_exists = session_identity_marker_path()
        .map(|p| p.exists())
        .unwrap_or(false);
    resolve_session_identity_gate(flag_on, marker_exists)
}

/// Header carrying the per-session loopback nonce that authenticates a
/// session's MCP client to the runner-local `/coord-mcp` proxy route.
/// Lowercase — HTTP header names are case-insensitive and axum's `HeaderMap`
/// keys are lowercased; the `.mcp.json` writer emits the canonical-case form.
pub(crate) const COORD_MCP_PROXY_KEY_HEADER: &str = "x-coord-mcp-proxy-key";

/// The coord HTTP base (no path, no trailing slash) plus WHICH resolution arm
/// produced it: env `COORD_HTTP_URL` → active profile's `coord_url` →
/// tier-aware default (prod coord on a `qontinui_account`-tier runner,
/// dev-localhost guess otherwise). Delegates to the shared policy fn
/// (`profiles::coord_base_with_source`) so a production-tier runner with
/// nothing configured dials prod coord, never dev-localhost — the 2026-07-16
/// coord-mcp 502 incident fix. Shared by every loopback proxy forwarder (the
/// `/mcp` JSON-RPC passthrough AND the nonce-gated claims/write passthroughs
/// in `mcp_api`) so they all resolve the coord base identically — a proxy
/// route must never re-derive it from env alone. The source is threaded into
/// proxy 502 error bodies so a misconfigured upstream self-diagnoses.
pub(crate) fn coord_base_url_with_source(
) -> (String, qontinui_runner_lib::profiles::CoordBaseSource) {
    let (base, source) = qontinui_runner_lib::profiles::coord_base_with_source();
    (base.trim_end_matches('/').to_string(), source)
}

/// [`coord_base_url_with_source`] without the source, for call sites that
/// only need the base.
pub(crate) fn coord_base_url() -> String {
    coord_base_url_with_source().0
}

/// The full coord `/mcp` endpoint URL + source: [`coord_base_url_with_source`]
/// with `/mcp` appended. Shared by the static-bearer `.mcp.json` writer (agent
/// path) and the loopback proxy forwarder (`mcp_api::coord_mcp_proxy_handler`).
pub(crate) fn coord_mcp_url_with_source() -> (String, qontinui_runner_lib::profiles::CoordBaseSource)
{
    let (base, source) = coord_base_url_with_source();
    (format!("{base}/mcp"), source)
}

/// [`coord_mcp_url_with_source`] without the source.
pub(crate) fn coord_mcp_url() -> String {
    coord_mcp_url_with_source().0
}

/// In-memory nonce registry for the loopback `/coord-mcp` proxy:
/// nonce → [`NonceBinding`] (the workdir it was provisioned into + the
/// [`ProxyPrincipal`] whose bearer it may inject). DEVICE bindings are mirrored
/// to the encrypted local store (Phase 3b) when `COORD_MCP_PERSIST_NONCES` is
/// not `0`, so the device set survives a runner rebuild/restart and an
/// already-written `.mcp.json` keeps validating (the MCP client never re-reads
/// the file). AGENT bindings are NEVER persisted (OQ3): a restarted runner has
/// no live agent session, so a restored agent nonce MUST hard-fail closed — the
/// handler 401s on the absent [`AGENT_TOKENS`] slot (process-global, never
/// persisted). With persistence disabled this degrades to the prior
/// process-lifetime-only behavior for device nonces too.
static PROXY_NONCES: OnceLock<Mutex<HashMap<String, NonceBinding>>> = OnceLock::new();

/// Per-agent live-token registry for the agent-proxy path: `agent_id` →
/// the agent's [`crate::agent_token::SharedToken`] slot. Built fresh at the
/// agent spawn site from the launch payload's JWT (there is no other live owner
/// — `agent_daemons::spawn_for_agent` has no production caller), refreshed
/// proactively from the agent's heartbeat loop, and dropped on teardown. A nonce
/// bound to [`ProxyPrincipal::Agent`] whose `agent_id` has no slot here is a
/// hard 401 — which is exactly what makes a restart (or a torn-down agent)
/// fail closed.
static AGENT_TOKENS: OnceLock<Mutex<HashMap<Uuid, crate::agent_token::SharedToken>>> =
    OnceLock::new();

fn agent_tokens() -> &'static Mutex<HashMap<Uuid, crate::agent_token::SharedToken>> {
    AGENT_TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register (or replace) the live-token slot for `agent_id`. Called at the agent
/// spawn site after building the slot from the launch payload's JWT.
pub(crate) fn register_agent_token(agent_id: Uuid, slot: crate::agent_token::SharedToken) {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .insert(agent_id, slot);
}

/// Look up the live-token slot for `agent_id` (clones the `Arc` out so the lock
/// is released immediately). `None` after teardown / before registration → the
/// proxy handler 401s, failing closed.
pub(crate) fn lookup_agent_token(agent_id: Uuid) -> Option<crate::agent_token::SharedToken> {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .get(&agent_id)
        .cloned()
}

/// Drop the live-token slot for `agent_id` on teardown so a torn-down agent's
/// nonce hard-fails closed. Idempotent.
pub(crate) fn remove_agent_token(agent_id: Uuid) {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .remove(&agent_id);
}

/// Guards [`restore_proxy_nonces_from_store`] so a second boot-restore (e.g. an
/// idempotent auto-start re-invocation) never re-loads over live in-memory
/// nonces minted since the first restore.
static PROXY_NONCES_RESTORED: OnceLock<()> = OnceLock::new();

fn proxy_nonces() -> &'static Mutex<HashMap<String, NonceBinding>> {
    PROXY_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Grace window (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart,
/// Change 3, defense in depth): a DEVICE nonce evicted by a same-workdir re-mint
/// stays valid for this long so an in-flight MCP client that cached it rides
/// through until it reconnects and re-reads the freshly-written `.mcp.json` (the
/// client never re-reads the file mid-connection, so a hard eviction 401s it the
/// instant the file is rewritten). Bounded — the accept-set widening lasts only
/// this window, and only for a device nonce the runner itself just superseded.
/// AGENT nonces are NEVER graced: they must hard-fail closed on re-mint/restart
/// (the scope-elevation non-goal, OQ3).
const NONCE_GRACE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// A device nonce kept transiently valid after eviction, with its expiry.
struct GracedNonce {
    expires_at: std::time::Instant,
}

/// Transient grace registry: an evicted DEVICE nonce → its expiry. Separate from
/// [`PROXY_NONCES`] so the live map stays the single source of truth for a
/// currently-provisioned nonce and grace never reaches disk (it is process-local
/// and intentionally forgotten across a restart — Change 1's adopt-on-disk path
/// owns cross-restart continuity).
static GRACED_NONCES: OnceLock<Mutex<HashMap<String, GracedNonce>>> = OnceLock::new();

fn graced_nonces() -> &'static Mutex<HashMap<String, GracedNonce>> {
    GRACED_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Move DEVICE nonces just evicted for `_workdir` into the grace registry with a
/// [`NONCE_GRACE_TTL`] expiry, opportunistically pruning expired entries so the
/// map stays bounded. Only device nonces are passed here (the caller filters);
/// agent nonces are dropped outright to fail closed.
fn grace_evicted_device_nonces(nonces: &[String]) {
    if nonces.is_empty() {
        return;
    }
    let now = std::time::Instant::now();
    let expires_at = now + NONCE_GRACE_TTL;
    let mut graced = graced_nonces().lock().expect("graced nonce map poisoned");
    graced.retain(|_, g| g.expires_at > now);
    for n in nonces {
        graced.insert(n.clone(), GracedNonce { expires_at });
    }
}

/// True iff `nonce` is a DEVICE nonce still inside its grace TTL (Change 3).
/// Lazily evicts it once expired so grace fails closed exactly at the deadline.
fn graced_nonce_is_valid(nonce: &str) -> bool {
    let now = std::time::Instant::now();
    let mut graced = graced_nonces().lock().expect("graced nonce map poisoned");
    match graced.get(nonce) {
        Some(g) if g.expires_at > now => true,
        Some(_) => {
            graced.remove(nonce);
            false
        }
        None => false,
    }
}

// ============================================================================
// Rotation forensics (plan 2026-07-27-coord-mcp-flake-remediation, Phase 4/R6)
// ============================================================================

/// Filename of the append-only rotation-forensics JSONL, written into the
/// dev-logs dir alongside the other `*.jsonl` streams. One line per nonce
/// mint / evict / grace / adopt and per `.mcp.json` write, so the NEXT
/// "transport died mid-session" incident is attributable from disk instead of
/// unlogged (the investigation's U8 gap: rotation events left zero trace).
const ROTATION_LOG_FILE: &str = "coord-mcp-rotations.jsonl";

/// How much of a nonce a forensics line may carry: the first 8 characters —
/// enough to correlate a line with an `.mcp.json` sighting, useless to
/// authenticate with (a full nonce is 64 chars of UUID hex).
const ROTATION_KEY_PREFIX_LEN: usize = 8;

/// The loggable prefix of `nonce` — NEVER the full key
/// ([`ROTATION_KEY_PREFIX_LEN`]).
fn rotation_key_prefix(nonce: &str) -> String {
    nonce.chars().take(ROTATION_KEY_PREFIX_LEN).collect()
}

/// Build one rotation-forensics JSONL line. Pure over its inputs (bar the
/// timestamp) so the shape — and the prefix-only guarantee — is unit-testable
/// without touching the filesystem.
fn rotation_log_line(event: &str, workdir: &str, nonce: &str, cause: &str) -> String {
    serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "workdir": workdir,
        "key_prefix": rotation_key_prefix(nonce),
        "cause": cause,
    })
    .to_string()
}

/// Shared `cause` text for a "grace" forensics line, naming the active TTL so
/// the log self-documents how long the evicted key stays acceptable.
fn rotation_grace_cause() -> String {
    format!("evicted device nonce graced {}s", NONCE_GRACE_TTL.as_secs())
}

/// Test-only redirect for the rotation log (process-global): `None` (the
/// default) silences file emission so the many nonce-minting unit tests never
/// write into the developer's real dev-logs dir.
#[cfg(test)]
static ROTATION_LOG_DIR_OVERRIDE: OnceLock<Mutex<Option<std::path::PathBuf>>> = OnceLock::new();

/// Switch file emission on for the test binary and return the ONE directory
/// every file-asserting forensics test shares. Idempotent by construction: the
/// first caller creates the dir and installs the override, every later caller
/// gets the same path back.
///
/// It must be shared, not per-test. The override is process-global while tests
/// run concurrently in one process, so a test that installed its OWN dir would
/// silently capture (and be captured by) any peer test's lines — whichever set
/// the override last wins, and the loser reads an empty or missing file. One
/// shared dir plus per-test filtering on unique workdirs is race-free instead.
#[cfg(test)]
fn rotation_log_test_dir() -> std::path::PathBuf {
    let mut slot = ROTATION_LOG_DIR_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("rotation log override poisoned");
    if let Some(existing) = slot.as_ref() {
        return existing.clone();
    }
    let dir = std::env::temp_dir().join(format!("coord-mcp-rot-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create rotation forensics test dir");
    *slot = Some(dir.clone());
    dir
}

/// Where the rotation log lives: the same dev-logs dir every other JSONL
/// stream resolves ([`crate::paths::get_dev_logs_dir`] — settings override
/// first, then the app-data default, instance-scoped for secondary runners).
/// `None` ⇒ skip file emission.
fn rotation_log_dir() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    {
        ROTATION_LOG_DIR_OVERRIDE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("rotation log override poisoned")
            .clone()
    }
    #[cfg(not(test))]
    {
        Some(crate::paths::get_dev_logs_dir())
    }
}

/// Record one rotation event: a `tracing::info!` line (so the spawn-log ring
/// sees it live) plus one appended JSONL line in the dev-logs dir (durable
/// across restarts — the ring is not). Best-effort and infallible by design:
/// forensics must never break provisioning, so an unresolvable dir or a failed
/// open/write is skipped silently (append-only `OpenOptions` shape cloned from
/// `tracing_layers.rs`'s spans stream). Callers MUST NOT hold the
/// nonce-registry lock — this does file I/O.
fn log_rotation_event(event: &str, workdir: &str, nonce: &str, cause: &str) {
    let prefix = rotation_key_prefix(nonce);
    info!("coord_mcp: rotation event={event} workdir={workdir} key_prefix={prefix} cause={cause}");
    let Some(dir) = rotation_log_dir() else {
        return;
    };
    use std::io::Write as _;
    let path = dir.join(ROTATION_LOG_FILE);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        // ONE `write_all` per event, newline included: concurrent emitters
        // (each opens its own append handle) must never interleave a line —
        // `writeln!` would issue the payload and the newline as separate
        // writes, which corrupts the JSONL under concurrency.
        let mut line = rotation_log_line(event, workdir, nonce, cause);
        line.push('\n');
        let _ = f.write_all(line.as_bytes());
    }
}

/// Minimum interval between `reject` forensics lines carrying the SAME key
/// prefix. Every other event is a discrete runner action (a mint, an eviction,
/// a file write) that cannot repeat in a tight loop; a reject fires on the
/// REQUEST path, so a client retrying against a dead key would otherwise append
/// one line per attempt and grow the log without bound. One line per key per
/// window attributes the incident just as well, and the suppressed repeats are
/// counted rather than silently dropped.
const REJECT_LOG_THROTTLE: std::time::Duration = std::time::Duration::from_secs(60);

/// Per-key-prefix reject-emission state: when a line was last written for this
/// prefix, and how many rejects have been suppressed since.
struct RejectThrottle {
    last_logged: std::time::Instant,
    suppressed: u64,
}

static REJECT_THROTTLES: OnceLock<Mutex<HashMap<String, RejectThrottle>>> = OnceLock::new();

/// Admit or suppress one reject for `prefix`. `Some(n)` ⇒ emit a line and
/// report that `n` rejects were suppressed since the previous one; `None` ⇒
/// stay silent, this prefix is inside its window.
///
/// A trailing suppressed count is lost if the rejects simply stop (nothing
/// arrives to carry it out) — acceptable: the incident is already on record via
/// the line that opened the window, and the alternative is a flush timer this
/// best-effort path has no business owning.
fn reject_throttle_admit(prefix: &str) -> Option<u64> {
    let now = std::time::Instant::now();
    let mut map = REJECT_THROTTLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("reject throttle map poisoned");
    // Opportunistic prune (the shape `grace_evicted_device_nonces` uses): a
    // long-lived runner can see many distinct dead keys, and an entry quiet for
    // well over a window has nothing left to throttle or report.
    map.retain(|_, t| now.duration_since(t.last_logged) < REJECT_LOG_THROTTLE * 10);
    // Early-return out of the `get_mut` borrow rather than matching it — the
    // insert below cannot coexist with a live `Option<&mut _>` scrutinee.
    if let Some(t) = map.get_mut(prefix) {
        if now.duration_since(t.last_logged) < REJECT_LOG_THROTTLE {
            t.suppressed += 1;
            return None;
        }
        t.last_logged = now;
        return Some(std::mem::take(&mut t.suppressed));
    }
    map.insert(
        prefix.to_string(),
        RejectThrottle {
            last_logged: now,
            suppressed: 0,
        },
    );
    Some(0)
}

/// The workdir a nonce is currently bound to, read WITHOUT mutating the
/// registry — unlike [`live_binding`], which lazily evicts an expired ephemeral
/// as a side effect. The reject forensics line runs on the request path after
/// the gate has already decided, so it must not change registry state.
fn known_workdir_for_nonce(nonce: &str) -> Option<String> {
    if nonce.is_empty() {
        return None;
    }
    proxy_nonces()
        .lock()
        .expect("proxy nonce map poisoned")
        .get(nonce)
        .map(|b| b.workdir.clone())
}

/// Record a coord-mcp proxy request REJECTED at the auth gate — the consumer
/// half of the rotation trail, and the line that makes the rest of it
/// answerable.
///
/// `mint` / `evict` / `grace` / `adopt` / `write` record what the runner did TO
/// a key. None of them records a key actually FAILING, so on their own they
/// show that rotations happened without ever pinning one to the incident it
/// caused — the U8 gap only half-closed. Join on `key_prefix`: an `evict` line
/// and a later `reject` line carrying the same prefix are the same key, and the
/// `evict` line supplies the workdir this one usually cannot (an unregistered
/// or already-evicted key has no binding left to look up).
///
/// Throttled per key prefix ([`REJECT_LOG_THROTTLE`]) because this runs on the
/// request path. Best-effort like every other forensics emission: it never
/// fails and never delays the 401 it accompanies. Callers MUST NOT hold the
/// nonce-registry lock.
pub(crate) fn log_proxy_nonce_rejected(nonce: Option<&str>, cause: &str) {
    let nonce = nonce.unwrap_or("");
    let prefix = rotation_key_prefix(nonce);
    let Some(suppressed) = reject_throttle_admit(&prefix) else {
        return;
    };
    // A still-registered nonce (bearer/principal mismatch, agent slot gone) can
    // name its own workdir; an unregistered or evicted one cannot — that is
    // what the prefix join is for.
    let workdir = known_workdir_for_nonce(nonce).unwrap_or_default();
    let cause = if suppressed > 0 {
        format!("{cause} [+{suppressed} identical rejects suppressed since the previous line]")
    } else {
        cause.to_string()
    };
    log_rotation_event("reject", &workdir, nonce, &cause);
}

/// [`log_proxy_nonce_rejected`] for an ASYNC caller. The emission opens and
/// appends to a file, and the proxy handler runs on the async executor — the
/// same reason the device-JWT read a few lines below it goes through
/// `spawn_blocking`. Detached and fire-and-forget: a 401 must never wait on
/// forensics, and a line lost to shutdown is the best-effort contract every
/// other emission on this path already carries.
pub(crate) fn spawn_log_proxy_nonce_rejected(nonce: Option<&str>, cause: impl Into<String>) {
    let nonce = nonce.map(str::to_owned);
    let cause = cause.into();
    tokio::task::spawn_blocking(move || log_proxy_nonce_rejected(nonce.as_deref(), &cause));
}

/// Project the live nonce map down to the DEVICE-only `nonce → workdir` shape
/// the encrypted store persists (OQ3): agent bindings are dropped so they never
/// reach disk. The store contract is unchanged (`HashMap<String, String>`), so
/// the persistence/restore seams and their tests stay green.
///
/// [`NonceLifetime::Ephemeral`] bindings are dropped too (plan 2026-07-17 §1/E).
/// A mint-route nonce is issued to a session the runner did not spawn, so it
/// must not outlive this process: the store has no expiry column, so a persisted
/// ephemeral nonce would silently restore as an UNBOUNDED one — laundering the
/// weaker class into the stronger one across a restart. Non-persistence is also
/// half of what makes the TTL meaningful (a leaked nonce cannot be replayed
/// against the next runner).
fn device_nonce_snapshot(map: &HashMap<String, NonceBinding>) -> HashMap<String, String> {
    map.iter()
        .filter(|(_, b)| b.principal == ProxyPrincipal::Device && !b.lifetime.is_ephemeral())
        .map(|(n, b)| (n.clone(), b.workdir.clone()))
        .collect()
}

/// True unless `COORD_MCP_PERSIST_NONCES` is explicitly set to `0` — persistence
/// is ON by default (resolved in the plan: the nonce is a loopback-only key, so
/// persisting it at rest is acceptable and is the only thing that keeps an
/// already-running agent alive across a restart). `=0` reverts to the prior
/// in-memory-only behavior.
fn nonce_persistence_enabled() -> bool {
    // In production: ON unless explicitly `=0`. In test builds: OFF unless
    // explicitly enabled — so the many nonce-minting unit tests never touch the
    // developer's real encrypted store; the persistence tests opt IN by setting
    // the env (under a serializing lock) to a temp store dir.
    match std::env::var("COORD_MCP_PERSIST_NONCES") {
        Ok(v) => v.trim() != "0",
        Err(_) => !cfg!(test),
    }
}

/// Mirror the live in-memory nonce map into the encrypted local store. No-op
/// when persistence is disabled. Best-effort: a store failure only `warn!`s —
/// the in-memory map stays authoritative for this process.
///
/// Opens the DEFAULT [`SecureStorage`] (honoring `QONTINUI_SECURE_STORAGE_DIR`)
/// and delegates to [`persist_proxy_nonces_with_store`]. Tests inject their own
/// store via the `_with_store` seam so they never touch the developer's real
/// encrypted store NOR the process-global env (which would race sibling tests
/// that read the default store, e.g. `auth::device_jwt_tests`).
fn persist_proxy_nonces(map: &HashMap<String, NonceBinding>) {
    if !nonce_persistence_enabled() {
        return;
    }
    match crate::secure_storage::SecureStorage::new() {
        Ok(store) => persist_proxy_nonces_with_store(&store, map),
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, proxy nonces not persisted: {e}");
        }
    }
}

/// Mirror `map` into the GIVEN store. The store is injected so the persistence
/// path is unit-testable against a temp-dir [`SecureStorage::with_path`] without
/// mutating `QONTINUI_SECURE_STORAGE_DIR` (which is process-global and pollutes
/// every other test that reads the default store). The `nonce_persistence_enabled`
/// gate is the CALLER's concern — handing a store IS the decision to persist.
fn persist_proxy_nonces_with_store(
    store: &crate::secure_storage::SecureStorage,
    map: &HashMap<String, NonceBinding>,
) {
    // OQ3: persist DEVICE bindings only — agent nonces must never reach disk.
    let device_only = device_nonce_snapshot(map);
    if let Err(e) = store.store_coord_mcp_nonces(&device_only) {
        warn!("coord_mcp: failed to persist proxy nonces: {e}");
    }
}

/// Restore persisted proxy nonces into the in-memory registry on boot (Phase
/// 3b). Idempotent + run-once: merges the persisted set UNDER any nonces already
/// minted this process (live mints win on key collision, which cannot happen in
/// practice — the persisted set predates this process). No-op when persistence
/// is disabled. Wire this into the same startup path as the other auto-start
/// tasks so already-written `.mcp.json` nonces keep validating post-restart.
/// Returns the number of nonces in the live registry after the restore merge
/// (0 when persistence is disabled, storage is unavailable, or nothing was
/// persisted). The count is surfaced by the boot task (plan 2026-07-07 Change 2
/// observability) so a future silent rotation — restore brought back 0 then
/// self-heal had to mint fresh — is visible in the logs.
pub(crate) fn restore_proxy_nonces_from_store() -> usize {
    if !nonce_persistence_enabled() {
        return 0;
    }
    if PROXY_NONCES_RESTORED.set(()).is_err() {
        // Already restored once this process — report the current live size so a
        // duplicate boot-task run still logs a coherent count.
        return proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .len();
    }
    let store = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s,
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, cannot restore proxy nonces: {e}");
            return 0;
        }
    };
    restore_proxy_nonces_from(&store)
}

/// Merge the persisted nonce set from the GIVEN store into the live in-memory
/// registry (live mints win on collision). The store is injected so the
/// restore path is unit-testable against a temp-dir store without the
/// run-once `PROXY_NONCES_RESTORED` guard or any global-env mutation. Returns
/// the live map size after the merge.
fn restore_proxy_nonces_from(store: &crate::secure_storage::SecureStorage) -> usize {
    let persisted = store.load_coord_mcp_nonces();
    if persisted.is_empty() {
        return proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .len();
    }
    let restored = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        for (nonce, workdir) in persisted {
            // Only DEVICE bindings are ever persisted (OQ3), so a restored entry
            // is unconditionally a Device principal. An agent nonce can never be
            // restored — its slot is process-global and gone after a restart, so
            // it would hard-fail closed anyway.
            map.entry(nonce).or_insert(NonceBinding {
                workdir,
                principal: ProxyPrincipal::Device,
                // Only Persistent bindings are ever written to the store
                // (`device_nonce_snapshot`), so a restored entry is
                // unconditionally Persistent — the restore cannot resurrect an
                // ephemeral mint-route nonce as an unbounded one.
                lifetime: NonceLifetime::Persistent,
                // The persisted store carries only (nonce, workdir), not the
                // session's tenant — so a restored nonce falls back to the
                // legacy default slot (`device_bearer_for(None)`), the pre-B3
                // behavior. A restored device session pre-dates this restart;
                // its original tenant is unrecoverable, and defaulting is the
                // safe (never cross-tenant) choice.
                session_tenant: None,
            });
        }
        map.len()
    };
    info!("coord_mcp: restored {restored} persisted proxy nonce(s) from secure storage");
    restored
}

/// Mint + register a fresh PERSISTENT per-session DEVICE proxy nonce for
/// `workdir`, returning it. Any prior persistent nonce registered for the same
/// workdir is evicted — a re-provision rewrites `.mcp.json`, so the old nonce is
/// unreachable and keeping it would only widen the accept set. The updated set is
/// mirrored to the encrypted store (Phase 3b) so it survives a restart.
///
/// This is the RUNNER-SPAWN path (the identity seam, the terminal chokepoint,
/// the boot self-heal). Its semantics are deliberately unchanged by plan
/// 2026-07-17 — see [`register_session_proxy_nonce`] for the mint-route path.
fn register_proxy_nonce(workdir: &str) -> String {
    let (nonce, snapshot) =
        mint_and_register_nonce(workdir, ProxyPrincipal::Device, NonceLifetime::Persistent);
    persist_proxy_nonces(&snapshot);
    nonce
}

/// Mint + register a fresh EPHEMERAL DEVICE proxy nonce for `workdir` — the
/// `/coord-mcp/provision-session` mint route's path (plan 2026-07-17 §1/E).
/// Identical principal and cwd-binding to [`register_proxy_nonce`]; the ONLY
/// differences are the [`NonceLifetime`] consequences: bounded expiry, revoked
/// by opting the machine out, and never persisted.
///
/// Deliberately NOT persisted — not even a store round-trip. Eviction is
/// class-scoped, so an ephemeral mint leaves the persisted (runner-spawn) set
/// byte-identical; writing it back would be pointless I/O whose only possible
/// effect is to clobber the set a concurrent restore just populated.
///
/// # Superseded-ephemeral window (intentional; per-nonce revoke deferred)
///
/// Removing per-workdir ephemeral eviction (the correct fix for the sibling-DoS,
/// where a bare mint for a shared cwd would 401 a live sibling's MCP client) has
/// a deliberate consequence: a SUPERSEDED ephemeral nonce — one whose session
/// ended or was re-minted — stays VALID until its [`EPHEMERAL_NONCE_TTL`]
/// (12h) expires. There is no per-nonce revoke. What bounds the exposure:
/// - per-nonce validity is capped by the TTL (a leaked nonce dies within a
///   working day and cannot be replayed against the next runner — it never
///   reaches disk), and
/// - instant GLOBAL revoke is deleting the opt-in marker
///   ([`session_identity_marker_path`]), re-checked per request via
///   [`session_identity_gate`] — the operator's real kill switch that
///   invalidates ALL ephemeral sessions at once.
///
/// Precise per-nonce revoke is INTENTIONALLY deferred to the credential-exposure
/// plan (`2026-07-17-coord-device-credential-exposure-and-authz-gaps`). Do NOT
/// shorten the TTL to paper over this — it would 401 live sessions mid-turn
/// (the MCP client never re-reads its config) — and do NOT re-add cwd-scoped
/// eviction, which caused the sibling-DoS.
fn register_session_proxy_nonce(workdir: &str) -> String {
    let (nonce, _snapshot) =
        mint_and_register_nonce(workdir, ProxyPrincipal::Device, NonceLifetime::ephemeral());
    nonce
}

/// Mint + register a fresh per-session proxy nonce bound to a specific AGENT for
/// `workdir`. Unlike [`register_proxy_nonce`] this is NOT persisted (OQ3) — an
/// agent nonce must hard-fail closed across a restart, which is automatic since
/// [`persist_proxy_nonces`] drops non-device bindings. The per-request bearer
/// comes from the agent's own [`AGENT_TOKENS`] slot, never the device JWT.
pub(crate) fn register_agent_proxy_nonce(workdir: &str, agent_id: Uuid) -> String {
    // Persistent lifetime = today's semantics (no expiry). It is NOT a disk
    // persistence decision: `device_nonce_snapshot` drops every agent binding
    // regardless, so an agent nonce still hard-fails closed across a restart.
    let (nonce, snapshot) = mint_and_register_nonce(
        workdir,
        ProxyPrincipal::Agent { agent_id },
        NonceLifetime::Persistent,
    );
    // Mirror to the store as a no-op for the agent entry (device entries in the
    // same snapshot, if any, are still persisted) — `persist_proxy_nonces`
    // filters agent bindings out, so this never writes the agent nonce to disk.
    persist_proxy_nonces(&snapshot);
    nonce
}

/// Mint a fresh nonce, evict any prior SAME-CLASS nonce for `workdir`, insert
/// it, and return `(nonce, snapshot)` — WITHOUT persisting. Split from the
/// persistence step so a test can mint and then mirror to an INJECTED store
/// ([`persist_proxy_nonces_with_store`]) instead of the default store reached
/// via the process-global `QONTINUI_SECURE_STORAGE_DIR`.
///
/// **Eviction rule (plan 2026-07-17 §1/E):**
/// - A **PERSISTENT** mint evicts the prior persistent-same-workdir nonce (the
///   runner-spawn re-provision case) and graces the evicted DEVICE ones. Within
///   the Persistent class this is byte-for-byte the previous behavior (every
///   runner-spawn nonce was Persistent, so "same workdir" and "same workdir +
///   same class" select the same set).
/// - An **EPHEMERAL** mint evicts **NOTHING**. Two DIFFERENT bare sessions
///   routinely share a cwd, and an ephemeral eviction is NOT graced (grace is
///   for runner-initiated re-provisions only), so removing a sibling ephemeral
///   nonce would 401 the other bare session's already-connected MCP client the
///   instant it was superseded — mid-session. Each ephemeral nonce is
///   independent and TTL-bounded, so a prior one is left to live out its own
///   TTL. The map stays bounded via the expired-ephemeral sweep below rather
///   than via eviction.
///
/// Either way an ephemeral mint never touches a persistent nonce (and vice
/// versa): the scoping exists so a BARE session minting for a cwd can never
/// evict the nonce of a live PTY terminal running in that same cwd — an
/// unprivileged mint-route call naming the operator's repo root must never 401
/// that terminal's MCP client. Two live nonces for one workdir is a sanctioned
/// state, and both map to the same workdir, so [`workdir_for_nonce`] stays
/// correct either way.
fn mint_and_register_nonce(
    workdir: &str,
    principal: ProxyPrincipal,
    lifetime: NonceLifetime,
) -> (String, HashMap<String, NonceBinding>) {
    // Two v4 UUIDs (~244 bits of randomness) — v4, NOT v7: the v7 prefix is a
    // timestamp, which would gut the entropy this nonce exists to provide.
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let ephemeral = lifetime.is_ephemeral();
    let now = std::time::Instant::now();
    // Freeze the session's tenant at mint time (B3): the active pin the
    // session was born under — the SAME source `stamp_session_tenant` reads
    // for the coord record. Resolved OUTSIDE the map lock (it is a file read)
    // so the registry mutex is never held across I/O. `None` on a
    // single-tenant install (→ legacy default slot at inject time).
    let session_tenant = crate::session::dual_write::resolve_active_tenant_id();
    // Forensics cause resolved BEFORE `principal` is moved into the map.
    let mint_cause = match (&principal, ephemeral) {
        (ProxyPrincipal::Device, false) => "persistent device mint (runner-spawn/re-provision)",
        (ProxyPrincipal::Device, true) => "ephemeral device mint (mint route)",
        (ProxyPrincipal::Agent { .. }, _) => "agent mint",
    };
    let (snapshot, evicted_device, evicted_agent) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // ONE pass does all pre-insert map maintenance (fused from three: an
        // expired-ephemeral sweep, a graceable collect, and a persistent-eviction
        // retain). The single `retain` closure decides removal AND collects the
        // grace set into `evicted_graceable`, so the map is walked once under the
        // lock. Semantics are byte-for-byte the prior three passes — see this
        // fn's doc for the eviction rule.
        let mut evicted_graceable: Vec<String> = Vec::new();
        let mut evicted_agent: Vec<String> = Vec::new();
        map.retain(|n, b| {
            // (1) Sweep EVERY expired ephemeral, whatever its workdir/class.
            // Because an ephemeral mint no longer evicts a prior same-workdir
            // ephemeral, expired ones would otherwise be reaped only lazily on
            // their own re-lookup ([`live_binding`]) — so a long-lived opted-in
            // runner minting across many distinct cwds could grow the map
            // unbounded. Cheap, bounded to mint frequency; never touches a
            // persistent nonce (no expiry) nor an unexpired ephemeral.
            if let NonceLifetime::Ephemeral { expires_at } = b.lifetime {
                if expires_at <= now {
                    return false;
                }
            }
            // (2) Class-scoped eviction. Only a PERSISTENT mint evicts, and only
            // the prior PERSISTENT same-workdir nonces (today's PTY re-provision
            // behavior — never an ephemeral, so the class-scoping holds). An
            // EPHEMERAL mint evicts NOTHING: two DIFFERENT bare sessions routinely
            // share a cwd, and an ephemeral eviction is not graced, so removing a
            // sibling ephemeral nonce would 401 the other session's
            // already-connected MCP client mid-session. The DEVICE nonces among
            // the evicted set are collected to ride a short grace TTL (Change 3) —
            // an in-flight client that cached one keeps validating until it
            // reconnects; agent nonces are NOT graced (they hard-fail closed on
            // re-mint), so they are dropped without being collected.
            if !ephemeral && b.workdir == workdir && !b.lifetime.is_ephemeral() {
                if b.principal == ProxyPrincipal::Device {
                    evicted_graceable.push(n.clone());
                } else {
                    evicted_agent.push(n.clone());
                }
                return false;
            }
            true
        });
        map.insert(
            nonce.clone(),
            NonceBinding {
                workdir: workdir.to_string(),
                principal,
                lifetime,
                session_tenant,
            },
        );
        grace_evicted_device_nonces(&evicted_graceable);
        (map.clone(), evicted_graceable, evicted_agent)
    };
    // Rotation forensics (Phase 4/R6) — emitted AFTER the registry lock is
    // released (file I/O must never run under it). The "grace" lines live here
    // rather than inside `grace_evicted_device_nonces` for the same reason:
    // both its callers invoke it under the registry lock, atomically with the
    // eviction, and moving the grace insert outside the lock would open a
    // window where an in-flight request finds its nonce neither live nor
    // graced. (Expired-ephemeral sweep removals above are deliberately NOT
    // logged: a TTL death is deterministic, not a rotation.)
    let grace_cause = rotation_grace_cause();
    for n in &evicted_device {
        log_rotation_event(
            "evict",
            workdir,
            n,
            "superseded by same-workdir persistent re-mint",
        );
        log_rotation_event("grace", workdir, n, &grace_cause);
    }
    for n in &evicted_agent {
        log_rotation_event(
            "evict",
            workdir,
            n,
            "superseded by same-workdir persistent re-mint (agent — fails closed, never graced)",
        );
    }
    log_rotation_event("mint", workdir, &nonce, mint_cause);
    (nonce, snapshot)
}

/// Evict every proxy nonce bound to `workdir` and persist the shrunken set.
/// Close-time cleanup for PER-SESSION workdirs (relay chat): unlike the stable
/// per-agent dirs, a per-session workdir is never reused, so the same-workdir
/// eviction inside [`mint_and_register_nonce`] never fires for it — without
/// this call its device nonce would stay valid (and persisted) forever.
/// Evicted device nonces ride the same grace TTL as a re-mint so an in-flight
/// client fails closed only after the window; agent nonces drop immediately.
pub(crate) fn evict_proxy_nonces_for_workdir(workdir: &str) {
    let (snapshot, evicted_device, evicted_agent) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        let evicted_device: Vec<String> = map
            .iter()
            .filter(|(_, b)| b.workdir == workdir && b.principal == ProxyPrincipal::Device)
            .map(|(n, _)| n.clone())
            .collect();
        if evicted_device.is_empty() && !map.values().any(|b| b.workdir == workdir) {
            return; // nothing bound to this workdir — skip the persist write
        }
        let evicted_agent: Vec<String> = map
            .iter()
            .filter(|(_, b)| b.workdir == workdir && b.principal != ProxyPrincipal::Device)
            .map(|(n, _)| n.clone())
            .collect();
        map.retain(|_, b| b.workdir != workdir);
        grace_evicted_device_nonces(&evicted_device);
        (map.clone(), evicted_device, evicted_agent)
    };
    // Rotation forensics — outside the lock (see `mint_and_register_nonce`).
    let grace_cause = rotation_grace_cause();
    for n in &evicted_device {
        log_rotation_event("evict", workdir, n, "per-session workdir closed");
        log_rotation_event("grace", workdir, n, &grace_cause);
    }
    for n in &evicted_agent {
        log_rotation_event(
            "evict",
            workdir,
            n,
            "per-session workdir closed (agent — fails closed, never graced)",
        );
    }
    persist_proxy_nonces(&snapshot);
}

/// Request header the runner proxy injects on forwarded `coord_*` calls: the
/// calling terminal's coord `agent_session_id`, so coord self-identifies the
/// caller deterministically instead of guessing the device's most-recent
/// session (session-fabric Phase 0). MUST match coord's `CALLER_SESSION_HEADER`.
pub(crate) const CALLER_SESSION_HEADER: &str = "x-coord-caller-session";

/// The session WORKDIR a registered proxy nonce was provisioned into (the
/// terminal's cwd / isolated worktree path). `None` for an empty, unregistered,
/// or no-longer-valid nonce ([`live_binding`]). Backs session-fabric Phase 0
/// caller self-identification: the proxy maps nonce → workdir → task_run_id →
/// coord `agent_session_id`.
pub(crate) fn workdir_for_nonce(nonce: &str) -> Option<String> {
    live_binding(nonce).map(|b| b.workdir)
}

/// Resolve `nonce`'s binding IF it is currently VALID, applying the
/// [`NonceLifetime`] rules (plan 2026-07-17 §1/E). The single chokepoint every
/// live-map lookup goes through, so expiry and revocation can never be enforced
/// on one path and forgotten on another.
///
/// - [`NonceLifetime::Persistent`] — always valid while registered. Today's
///   behavior, untouched: a runner-spawned session pays nothing (no clock read,
///   no filesystem stat) and can never be revoked out from under itself.
/// - [`NonceLifetime::Ephemeral`] — valid only while (a) inside its TTL and
///   (b) the machine is STILL opted in. Expiry LAZILY evicts, so the map stays
///   bounded and the deadline fails closed exactly on time. An opt-OUT does NOT
///   evict: revocation must be reversible — re-creating the marker restores a
///   live session's identity, whereas evicting would kill it permanently (the
///   MCP client never re-reads its config, so it could never pick up a re-mint).
///
/// The map lock is released before the gate's filesystem check — a proxy request
/// must never hold the registry lock across I/O.
fn live_binding(nonce: &str) -> Option<NonceBinding> {
    if nonce.is_empty() {
        return None;
    }
    let binding = proxy_nonces()
        .lock()
        .expect("proxy nonce map poisoned")
        .get(nonce)
        .cloned()?;
    match binding.lifetime {
        NonceLifetime::Persistent => Some(binding),
        NonceLifetime::Ephemeral { expires_at } => {
            if expires_at <= std::time::Instant::now() {
                proxy_nonces()
                    .lock()
                    .expect("proxy nonce map poisoned")
                    .remove(nonce);
                return None;
            }
            session_identity_gate().is_ok().then_some(binding)
        }
    }
}

/// True iff `nonce` is a currently-registered AND currently-valid per-session
/// proxy key ([`live_binding`] — expiry + revocation applied) OR a DEVICE nonce
/// still inside its post-eviction grace TTL (Change 3). The live-map lock is
/// taken and released before the grace check so the two maps are never held at
/// once.
pub(crate) fn proxy_nonce_is_valid(nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    live_binding(nonce).is_some() || graced_nonce_is_valid(nonce)
}

/// Resolve the [`ProxyPrincipal`] a registered nonce is bound to. `None` for an
/// empty or unregistered nonce — the handler treats that as a 401. The handler
/// resolves the principal BEFORE reading any bearer, so the bearer it injects is
/// chosen by the binding (device JWT vs the agent's own JWT) rather than the
/// other way around.
pub(crate) fn proxy_principal_for_nonce(nonce: &str) -> Option<ProxyPrincipal> {
    if nonce.is_empty() {
        return None;
    }
    let live = live_binding(nonce).map(|b| b.principal);
    // Grace fallback (Change 3): only DEVICE nonces are ever graced, so a graced
    // hit resolves to a Device principal — the handler then injects the live
    // device JWT and `proxy_request_gate` still enforces device-nonce ⇒
    // device-bearer (no scope-elevation surface).
    live.or_else(|| graced_nonce_is_valid(nonce).then_some(ProxyPrincipal::Device))
}

/// The tenant a DEVICE proxy nonce was provisioned under (B3), frozen at mint
/// time on its [`NonceBinding`]. The `/coord-mcp` proxy handler feeds this into
/// [`crate::auth::device_bearer_for`] so it injects the SESSION's tenant's
/// device JWT — never blindly the legacy `access_token` slot (the default
/// binding), which is the cross-tenant split-brain B3 closes.
///
/// `None` means "inject the legacy default slot" (`device_bearer_for(None)`),
/// which is correct for every case that returns it: a nonce provisioned on a
/// single-tenant install (no active pin), a restored nonce (tenant not
/// persisted), or a graced nonce (no live binding). A miss for a live
/// NON-default tenant is NOT this function's concern — that is enforced inside
/// `device_bearer_for`, which returns `None` (→ the proxy's refresh/401 path)
/// on a non-default slot miss rather than falling back to the legacy slot.
pub(crate) fn proxy_session_tenant_for_nonce(nonce: &str) -> Option<Uuid> {
    live_binding(nonce).and_then(|b| b.session_tenant)
}

/// Pre-forward gate for the loopback `/coord-mcp` proxy route. Pure over its
/// inputs (no I/O) so the 401 paths are unit-testable without a live coord:
///
/// 1. `nonce` must be a registered per-session key ([`proxy_nonce_is_valid`])
///    — stops other local processes borrowing the runner's coord identity.
/// 2. `bearer` (the resolved per-request token — the live device JWT for a
///    [`ProxyPrincipal::Device`] nonce, the agent's own refreshed JWT for an
///    [`ProxyPrincipal::Agent`] nonce) must be present and decode a `sub_type`
///    matching the bound `principal` ([`ProxyPrincipal::expected_sub_type`]).
///
/// The `nonce → principal → sub_type` chain is the structural backstop against
/// the scope-elevation trap: a device nonce can only ever forward a device
/// token and an agent nonce only ever an agent token — neither can elevate into
/// the other. The handler resolves `principal` from the nonce BEFORE picking the
/// bearer, so this gate only re-validates that pairing.
///
/// `Ok(())` means: forward with `Authorization: Bearer <bearer>`.
pub(crate) fn proxy_request_gate(
    nonce: Option<&str>,
    bearer: Option<&str>,
    principal: &ProxyPrincipal,
) -> Result<(), (u16, String)> {
    if !proxy_nonce_is_valid(nonce.unwrap_or("")) {
        return Err((
            401,
            "missing or unrecognized X-Coord-Mcp-Proxy-Key".to_string(),
        ));
    }
    let bearer = match bearer {
        Some(b) if !b.trim().is_empty() => b,
        _ => {
            return Err((
                401,
                "no live JWT available for this proxy session".to_string(),
            ));
        }
    };
    let expected = principal.expected_sub_type();
    match jwt_unverified_claim(bearer, "sub_type").as_deref() {
        Some(st) if st == expected => Ok(()),
        other => Err((
            401,
            format!(
                "proxy bearer sub_type={other:?} does not match the nonce's \
                 bound principal (expected {expected:?}) — refusing to forward"
            ),
        )),
    }
}

/// Phase 3 (terminal-autonomy-survives-logout): maximum TOTAL time the
/// DEVICE-path coord-mcp proxy will wait for the device-JWT refresher to
/// re-mint a momentarily-missing token before it degrades to an actionable
/// retry error. Bounded tightly — a proxy request must NEVER block
/// indefinitely on a credential gap. The device JWT re-mints from the
/// preserved Cognito session in seconds (Phase 1), so this smooths the common
/// re-mint-window / transient-backoff gap (Phases 1-2) without hanging.
pub(crate) const DEVICE_JWT_REMINT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Poll interval while waiting for the [`DEVICE_JWT_REMINT_WAIT`] re-mint.
pub(crate) const DEVICE_JWT_REMINT_POLL: std::time::Duration =
    std::time::Duration::from_millis(250);

/// The actionable, retry-shaped error the DEVICE-path proxy returns when the
/// device JWT is STILL missing after the bounded [`DEVICE_JWT_REMINT_WAIT`].
///
/// Distinct from the gate's hard 401s (bad/absent nonce, scope-elevation): this
/// is a TRANSIENT credential gap, not an auth failure — the autonomous session
/// is alive and the refresher is re-minting, so the caller should simply retry.
/// `503` (the canonical "temporarily unavailable, retry" code) keeps it an
/// error the MCP client will not mistake for success while signalling
/// retry-ability; the message is human/agent-actionable rather than a bare
/// "no live JWT available". The nonce/scope FAIL-CLOSED 401s in
/// [`proxy_request_gate`] are unchanged — only the missing-device-JWT case
/// degrades to this.
pub(crate) fn device_jwt_refreshing_error() -> (u16, String) {
    (
        503,
        "coord credential refreshing — autonomous session will resume; \
         retry shortly"
            .to_string(),
    )
}

/// Bounded poll for a usable token: returns as soon as `read_usable` yields a
/// non-empty token, or `None` once `total` elapses. Generic over the reader so
/// the bound/termination behavior is unit-testable without a live AuthManager
/// or a real 5s wait. NEVER blocks indefinitely — the deadline is checked every
/// `interval`.
async fn await_remint_with<F, Fut>(
    mut read_usable: F,
    total: std::time::Duration,
    interval: std::time::Duration,
) -> Option<String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if let Some(t) = read_usable().await {
            if !t.trim().is_empty() {
                return Some(t);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Read the device JWT ONLY if it is freshly usable (present, non-empty, and
/// not stale per [`AuthManager::device_jwt_needs_refresh`]). Filesystem I/O, so
/// it runs on a blocking thread off the async executor — the same discipline
/// the proxy handler's first read uses. Used by [`await_device_jwt_remint`] to
/// detect "a usable JWT is now present" after a refresher kick, and by the
/// CI-node reporter (`ci_node::reporting`) as the cheap fresh-read half of
/// its bearer resolution.
pub(crate) async fn read_usable_device_jwt() -> Option<String> {
    read_usable_device_jwt_for(None).await
}

/// Tenant-selecting variant of [`read_usable_device_jwt`] (B3). Selects the
/// bearer via [`crate::auth::device_bearer_for`] for `tenant` instead of the
/// legacy `access_token` slot, so the DEVICE proxy path presents the SESSION's
/// tenant credential. The freshness gate ([`AuthManager::device_jwt_needs_refresh`])
/// is unchanged — it is the device-wide "is this runner broadly authed" check.
///
/// CRITICAL (B3 security invariant): for a NON-default `tenant`, `device_bearer_for`
/// returns `None` on a slot miss — it NEVER falls back to the legacy default
/// slot. So a non-default session whose tenant has no keyring slot resolves to
/// `None` here, and the caller degrades to the proxy's refresh/401 path rather
/// than silently acting as the default tenant. `None` ⇒ legacy default slot
/// (the pre-B3 behavior every existing default-binding session relies on).
pub(crate) async fn read_usable_device_jwt_for(tenant: Option<Uuid>) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let am = crate::auth::AuthManager::new();
        match am.device_jwt_needs_refresh() {
            Ok(false) => {
                crate::auth::device_bearer_for(tenant.as_ref()).filter(|t| !t.trim().is_empty())
            }
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Phase 3 graceful-degrade for the DEVICE proxy path: when the device JWT is
/// momentarily absent, KICK the refresher (so it re-mints immediately from the
/// preserved Cognito session instead of waiting out its sleep) and wait a
/// tightly-bounded [`DEVICE_JWT_REMINT_WAIT`] for a usable JWT to appear.
/// Returns the fresh token if one re-mints within the bound, else `None` (the
/// caller then degrades to [`device_jwt_refreshing_error`]). Agent-bound proxy
/// requests do NOT use this — they refresh via their own `AGENT_TOKENS` slot.
pub(crate) async fn await_device_jwt_remint() -> Option<String> {
    await_device_jwt_remint_for(None).await
}

/// Tenant-selecting variant of [`await_device_jwt_remint`] (B3). Polls
/// [`read_usable_device_jwt_for`] for `tenant` after kicking the refresher, so
/// the bounded re-mint wait resolves the SESSION's tenant credential. For a
/// NON-default `tenant` with no keyring slot this stays `None` for the whole
/// window (`device_bearer_for` never serves the legacy slot for it) — the
/// handler then degrades to [`device_jwt_refreshing_error`], never the default
/// tenant's token.
pub(crate) async fn await_device_jwt_remint_for(tenant: Option<Uuid>) -> Option<String> {
    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    await_remint_with(
        move || read_usable_device_jwt_for(tenant),
        DEVICE_JWT_REMINT_WAIT,
        DEVICE_JWT_REMINT_POLL,
    )
    .await
}

/// Resolve the runner's ACTUALLY-BOUND local API port for loopback URLs:
/// the managed `AppState.api_port` (set at bind time) via the process-global
/// `AppHandle`. Returns `None` — fail-closed, Phase 3a — when no Tauri runtime
/// / managed `AppState` is reachable, rather than degrading to the bootstrap
/// [`crate::mcp::types::get_mcp_api_port`] default (`MCP_API_PORT=9876`). That
/// default is correct only by luck on a single-runner box and silently WRONG on
/// any secondary/temp runner bound to a different port (the F1 root cause: a
/// `:9876` URL written into a config whose live proxy is on `:9877`). Callers
/// MUST treat `None` as "refuse to write a proxy config" — a dead config that
/// looks valid is worse than an absent one.
pub(crate) fn resolve_bound_api_port() -> Option<u16> {
    if let Some(app) = crate::tauri_app_handle::current() {
        use tauri::Manager;
        if let Some(state) = app.try_state::<std::sync::Arc<crate::commands::AppState>>() {
            return Some(crate::mcp::types::runner_api_port(state.inner()));
        }
    }
    None
}

/// Write the DEVICE-path `.mcp.json`: an `http`-transport server pointing at
/// the runner's own loopback `/coord-mcp` proxy on the ACTUALLY-BOUND API
/// port, authenticated by a freshly-minted per-session nonce — and NO baked
/// bearer. The proxy injects a live device JWT per request, so the config
/// survives the 4h token TTL that kills static-bearer configs in sessions
/// that outlive their snapshot (the MCP client never re-reads `.mcp.json`).
pub(crate) fn write_coord_mcp_proxy_config(primary_wt: &str, bound_port: u16) {
    let nonce = register_proxy_nonce(primary_wt);
    write_mcp_json(primary_wt, &coord_mcp_proxy_config_json(bound_port, &nonce));
}

/// THE coord-mcp proxy config document — the single writer of this JSON shape
/// (plan 2026-07-17 §2). Every emitter goes through here: the in-cwd
/// `.mcp.json` writers ([`write_coord_mcp_proxy_config`],
/// [`write_coord_mcp_agent_proxy_config`]), the app-data `--mcp-config` file the
/// identity seam delivers ([`provision_coord_mcp_config_file`]), and the
/// `/coord-mcp/provision-session` mint route
/// ([`provision_session_proxy_config`]). Four call sites had independently
/// duplicated this literal; one writer means the loopback-URL/nonce-header
/// contract cannot drift between the path a runner-spawned session gets and the
/// path a bare session gets.
///
/// Note what is NOT here: an `Authorization` bearer. The proxy injects a live
/// per-request one keyed off the nonce's principal — baking a static token is
/// the failure this shape exists to avoid.
fn coord_mcp_proxy_config_json(bound_port: u16, nonce: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{bound_port}/coord-mcp"),
                "headers": {
                    "X-Coord-Mcp-Proxy-Key": nonce,
                }
            }
        }
    })
}

/// Write the AGENT-path `.mcp.json`: identical shape to
/// [`write_coord_mcp_proxy_config`] (loopback proxy URL on the bound port + a
/// per-session nonce header, NO baked bearer), but the nonce is bound to
/// [`ProxyPrincipal::Agent`] for `agent_id` — so the proxy injects THAT agent's
/// own refreshed JWT (from [`AGENT_TOKENS`]) per request, never the device JWT.
/// This is what lets an agent session outlive the 4h agent-JWT TTL (the static
/// bake at the old spawn site silently lost coord-mcp at expiry).
///
/// The caller MUST register the agent's [`crate::agent_token::SharedToken`] via
/// [`register_agent_token`] (and drive proactive refresh) — an agent nonce with
/// no live slot hard-fails closed (401).
pub(crate) fn write_coord_mcp_agent_proxy_config(
    primary_wt: &str,
    bound_port: u16,
    agent_id: Uuid,
) {
    let nonce = register_agent_proxy_nonce(primary_wt, agent_id);
    write_mcp_json(primary_wt, &coord_mcp_proxy_config_json(bound_port, &nonce));
}

/// Filename of the Phase-1a degraded-only breadcrumb dropped into a session
/// workdir when coord-mcp provisioning is degraded (no JWT, unresolvable port,
/// or a failed reachability probe). Referenced by the `/gate` skill + CLAUDE.md.
pub(crate) const COORD_MCP_STATUS_FILE: &str = ".coord-mcp-status";

/// Env var the runner injects at spawn carrying the absolute path of a
/// runner-owned coord-mcp `--mcp-config` file (the DEVICE loopback-proxy shape).
/// The identity shim's `claude` wrapper reads it and appends
/// `--mcp-config $that` to the real argv — the universal delivery seam that gives
/// EVERY device-scope session (button-spawned, restore-re-spawned, hand-typed in
/// an arbitrary cwd, fresh install) coord-mcp with no `.mcp.json` in the cwd and
/// no user setup. Empty/unset ⇒ the shim appends nothing (fail-open — the session
/// simply has no coord-mcp, never a broken/FAILED server). Parallel to
/// [`crate::session::claude_hook::CLAUDE_SETTINGS_ENV`].
pub(crate) const MCP_CONFIG_ENV: &str = "QONTINUI_MCP_CONFIG";

/// Write a SHORT, single-line degraded breadcrumb into `workdir` (Phase 1a).
/// Emitted ONLY when coord-mcp is degraded — a healthy session writes nothing
/// (the file is removed by [`clear_degraded_breadcrumb`] on a successful probe),
/// so its mere presence is the signal. Best-effort: a write failure only logs.
pub(crate) fn write_degraded_breadcrumb(workdir: &str, reason: &str) {
    let line =
        format!("coord-mcp UNREACHABLE ({reason}) — gate registration degraded; use /gate\n");
    let path = Path::new(workdir).join(COORD_MCP_STATUS_FILE);
    if let Err(e) = std::fs::write(&path, line) {
        warn!("coord_mcp: failed to write degraded breadcrumb in {workdir}: {e}");
    }
}

/// Remove a stale degraded breadcrumb once coord-mcp is confirmed reachable, so
/// a session that recovered (e.g. a reconcile fixed the port) does not keep
/// showing a stale UNREACHABLE marker. Best-effort + idempotent (absent = ok).
fn clear_degraded_breadcrumb(workdir: &str) {
    let path = Path::new(workdir).join(COORD_MCP_STATUS_FILE);
    let _ = std::fs::remove_file(path);
}

/// One-shot, non-blocking coord-mcp reachability probe (Phase 1a). Fires a
/// `tools/list` JSON-RPC at the configured loopback proxy
/// (`http://127.0.0.1:<port>/coord-mcp`) carrying the session's nonce header,
/// with a short timeout, on a DETACHED thread so it never blocks or panics
/// session provisioning. On a non-2xx / transport failure it drops the
/// degraded breadcrumb; on success it clears any stale one and writes nothing.
pub(crate) fn probe_and_breadcrumb_proxy(workdir: &str, port: u16) {
    // Resolve the nonce we just wrote for this workdir so the probe authenticates
    // exactly as the session's MCP client will.
    let nonce = {
        let map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // The RUNNER-SPAWN nonce specifically: the caller just wrote one for
        // this workdir, and a bare session may hold an ephemeral nonce for the
        // same cwd. Probing with the ephemeral one would make this probe's
        // verdict depend on the opt-in marker — and a revoked ephemeral nonce
        // would 401, dropping a bogus "UNREACHABLE" breadcrumb into the user's
        // cwd for a config that is in fact healthy.
        map.iter()
            .find(|(_, b)| b.workdir.as_str() == workdir && !b.lifetime.is_ephemeral())
            .map(|(n, _)| n.clone())
    };
    let Some(nonce) = nonce else {
        return; // no nonce → nothing to probe against (already handled upstream)
    };
    let workdir = workdir.to_string();
    std::thread::spawn(move || {
        let url = format!("http://127.0.0.1:{port}/coord-mcp");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        });
        let client = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(_) => return, // can't build a client — skip silently (best-effort)
        };
        let reachable = client
            .post(&url)
            .header(COORD_MCP_PROXY_KEY_HEADER, &nonce)
            .json(&body)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if reachable {
            clear_degraded_breadcrumb(&workdir);
        } else {
            write_degraded_breadcrumb(
                &workdir,
                &format!("port :{port} probe failed (dead port | 401 stale nonce | coord down)"),
            );
        }
    });
}

/// Restrict `path` so only the owning user can read it.
///
/// Every file this module writes carries a coord proxy **nonce** — a
/// replayable credential that authenticates as this device (or agent) against
/// coord. They were written with default permissions, i.e. world-readable, and
/// they are long-lived: a config minted 2026-07-13 was still valid and readable
/// by any local process on 2026-07-21. Any process running as any local user
/// could read one and act with this principal's authority.
///
/// **Best-effort by design — a failure here is logged, never fatal.** Losing
/// coord-mcp delivery is a strictly worse outcome than a permissive file, and
/// hard-failing would break every session spawn on any filesystem where the
/// call misbehaves (network shares, exotic ACL states). The credential is no
/// worse off than before the call.
///
/// `is_dir` additionally makes the restriction inheritable, so files created
/// inside later are covered. Pass `false` for a file whose PARENT must stay
/// accessible — notably `<repo>/.mcp.json`, whose parent is a repo working-tree
/// root that other tooling and the operator must keep using.
fn restrict_to_owner(path: &Path, is_dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if is_dir { 0o700 } else { 0o600 };
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            warn!(
                "coord_mcp: could not restrict {} to {mode:o}: {e} — \
                 the file remains readable by other local users",
                path.display()
            );
        }
    }

    #[cfg(windows)]
    {
        // No std API sets a Windows DACL (`set_permissions` only toggles the
        // read-only attribute), and the `windows` crate route needs a
        // hand-built DACL. `icacls` is a built-in, is what an operator would
        // run by hand, and keeps this reviewable — so no new dependency.
        //
        // /inheritance:r drops inherited ACEs (otherwise the permissive parent
        // ACL keeps granting access); /grant:r replaces rather than adds.
        let Some(user) = std::env::var("USERNAME").ok().filter(|u| !u.is_empty()) else {
            warn!(
                "coord_mcp: USERNAME unset — cannot restrict {}; \
                 it remains readable by other local users",
                path.display()
            );
            return;
        };
        let grant = if is_dir {
            format!("{user}:(OI)(CI)(F)")
        } else {
            format!("{user}:(F)")
        };
        match std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(&grant)
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => warn!(
                "coord_mcp: icacls could not restrict {}: {} — \
                 it remains readable by other local users",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => warn!(
                "coord_mcp: could not run icacls for {}: {e} — \
                 it remains readable by other local users",
                path.display()
            ),
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, is_dir);
    }
}

fn write_mcp_json(primary_wt: &str, mcp_config: &serde_json::Value) {
    let mcp_path = Path::new(primary_wt).join(".mcp.json");
    match std::fs::write(
        &mcp_path,
        serde_json::to_string_pretty(mcp_config).unwrap_or_default(),
    ) {
        Ok(()) => {
            // File only — the parent is a repo working-tree root that other
            // tooling and the operator must keep using.
            restrict_to_owner(&mcp_path, false);
            info!("coord_mcp: wrote .mcp.json for coord-mcp in {}", primary_wt);
            // Rotation forensics (Phase 4/R6): every write of this file is a
            // client-visible rotation candidate — record which key it now
            // carries. Extraction is infallible-by-shape (the single writer
            // `coord_mcp_proxy_config_json` always sets the header); an
            // unexpected shape logs an empty prefix rather than nothing.
            let key = mcp_config
                .pointer("/mcpServers/coord-mcp/headers/X-Coord-Mcp-Proxy-Key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            log_rotation_event(
                "write",
                primary_wt,
                key,
                ".mcp.json rewritten (proxy shape)",
            );
        }
        Err(e) => {
            warn!(
                "coord_mcp: failed to write .mcp.json in {}: {e}",
                primary_wt
            );
        }
    }
}

/// Provision `.mcp.json` for a runner-spawned session that did NOT arrive with a
/// coord-minted agent JWT — i.e. gate-continuation terminals. Uses the runner's
/// own live **device** JWT (the same `SubType::Device` EdDSA token the coord
/// `/mcp` relay presents) so the continuation can use `coord_register_gate` over
/// MCP and `coord-acting-bearer.sh` (which mints an acting-user Service token
/// from it). This closes the reach gap where continuations — a primary place
/// follow-up gates get registered — otherwise fell back to the operator-bearer
/// stopgap (plan 2026-06-09-provision-coord-mcp-on-all-runner-spawned-sessions).
///
/// Two guards the agent-spawn path does not need (it always writes a coord agent
/// JWT into a fresh worktree):
/// 1. **sub_type guard.** The `access_token` slot can hold a *Cognito* token
///    rather than the coord device JWT on some pairing tiers
///    (`device_jwt_refresher`); a Cognito bearer would 401 against coord's EdDSA
///    verifier. Write only when the bearer decodes as `sub_type ∈ {device,
///    agent}`; otherwise log + skip (never write a non-verifying bearer).
/// 2. **non-clobber guard.** A continuation can degrade to a canonical repo
///    checkout (worktree mode off / acquire declined), which may already hold
///    the operator's own `.mcp.json`. Overwrite only when the file is absent or
///    is solely our `coord-mcp` config (see [`coord_mcp_safe_to_write`]).
// `pub(crate)` so the operator-opened-tab entry point
// (`commands::terminal::terminal_create`) reuses this exact helper — closing the
// last dark runner-spawned session kind (plan
// 2026-06-09-provision-coord-mcp-operator-tabs). Both modules compile into the
// runner bin crate, so `pub(crate)` is the minimal visibility that reaches it.
pub(crate) fn provision_coord_mcp_for_session(workdir: &str, bound_port: Option<u16>) {
    let jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            info!(
                "coord_mcp: no device JWT in access_token slot — skipping \
                 coord-mcp provisioning for {workdir}"
            );
            // 1a breadcrumb: a session with no device JWT cannot reach coord —
            // surface it so the agent self-routes to `/gate` instead of finding
            // a silently-absent MCP tool.
            write_degraded_breadcrumb(
                workdir,
                "no device JWT in runner access_token slot — coord-mcp not provisioned",
            );
            return;
        }
    };

    provision_coord_mcp_with_jwt(workdir, &jwt, bound_port);
}

/// Apply an already-resolved bearer to `workdir`'s `.mcp.json`, enforcing the two
/// guards: the bearer must decode `sub_type ∈ {device, agent}` (never write a
/// non-coord-verifying token), and the non-clobber / no-downgrade guard
/// ([`coord_mcp_safe_to_write`]) must allow it. Split out from
/// [`provision_coord_mcp_for_session`] so the write-decision orchestration is
/// unit-testable with a synthetic JWT — the live credential lives in the
/// encrypted `AuthManager` slot, which a unit test cannot seed deterministically
/// (and which would otherwise make the test pass/fail by whether the host happens
/// to be paired).
///
/// Device/agent split (live-token-proxy plan): `sub_type=device` emits the
/// loopback PROXY shape ([`write_coord_mcp_proxy_config`] on `bound_port`) so
/// the session reads a live device JWT per request; `sub_type=agent` keeps the
/// static-bearer shape UNCHANGED — agent JWTs are deliberately narrower than
/// the device JWT the proxy injects, so an agent session must never be routed
/// through the proxy (scope elevation).
fn provision_coord_mcp_with_jwt(workdir: &str, jwt: &str, bound_port: Option<u16>) {
    let sub_type = jwt_unverified_claim(jwt, "sub_type");
    match sub_type.as_deref() {
        Some("device") | Some("agent") => {}
        other => {
            info!(
                "coord_mcp: access_token bearer is not a coord device/agent \
                 JWT (sub_type={other:?}) — skipping coord-mcp provisioning for \
                 {workdir} (would 401 against coord's EdDSA verifier)"
            );
            return;
        }
    }

    if !coord_mcp_safe_to_write(workdir) {
        info!(
            "coord_mcp: {workdir}/.mcp.json already holds a non-coord-mcp \
             config — leaving it untouched (no coord-mcp provisioning)"
        );
        return;
    }

    if sub_type.as_deref() == Some("device") {
        // Phase 3a — fail-closed on an unknown bound port. The device path
        // writes a loopback proxy URL; if we can't resolve the ACTUALLY-BOUND
        // port we must NOT write a config pointing at the bootstrap default
        // (`:9876`), which is wrong on any secondary/temp runner and produces a
        // dead-but-valid-looking config (the F1 root cause). Refuse, warn, and
        // drop a 1a breadcrumb so the agent routes to `/gate`.
        let port = match bound_port {
            Some(p) => p,
            None => {
                warn!(
                    "coord_mcp: refusing to write a proxy .mcp.json for {workdir} — \
                     the bound API port is unresolvable (no managed AppState); a \
                     bootstrap-default port would be dead on a secondary runner. \
                     Run `coord doctor` / reprovision once the runtime is up."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "bound API port unresolvable — proxy config NOT written (would point at a dead port)",
                );
                return;
            }
        };
        write_coord_mcp_proxy_config(workdir, port);
        // 1a — one-shot, non-blocking reachability probe; writes a breadcrumb
        // only on failure, nothing on success (discoverability without clutter).
        probe_and_breadcrumb_proxy(workdir, port);
    } else {
        // AGENT path. In production this arm is effectively unreachable: the
        // only callers (`provision_coord_mcp_for_session` ← gate-continuation +
        // the `acquire_for_terminal` terminal chokepoint) read the bearer from
        // the runner's DEVICE access_token slot, which never holds an agent JWT
        // (agent JWTs are minted by coord and delivered in `LaunchPayload.jwt`,
        // provisioned at the spawn site, not here). It is kept defensively and
        // is exercised only by `provision_with_jwt_orchestration`.
        //
        // Even so we use the per-agent PROXY shape (delete-over-deprecate): bake
        // a `SharedToken` from the JWT claims, register it, and write the agent
        // proxy config. LIMITATION: this path has NO heartbeat driver, so the
        // slot is refreshed ONLY lazily on the request path (`maybe_refresh` in
        // the handler). For a steadily-used MCP client that is sufficient (the
        // 30-min margin ≫ inter-call gap); an idle agent that goes >TTL between
        // coord-mcp calls could still expire (coord's refresh endpoint rejects
        // an already-expired token — OQ4). The spawn-site path (with the 30s
        // heartbeat refresh) is the one that matters and never expires.
        // Fail closed on a missing/unparseable `sub` (the agent_id): registering a
        // `Uuid::nil()`-keyed slot would let two malformed-JWT provisions collide on
        // the same nil key (the second overwriting the first's slot). The trusted
        // spawn site (`agent_runtime::run_agent_subprocess`) keys on `payload.agent_id`
        // and never reaches here; this arm only ever sees a well-formed agent JWT in
        // tests, so a nil `sub` means a malformed token — refuse rather than register
        // an ambiguous slot.
        let agent_id = match jwt_unverified_claim(jwt, "sub").and_then(|s| Uuid::parse_str(&s).ok())
        {
            Some(id) => id,
            None => {
                warn!(
                    "coord_mcp: refusing to write an AGENT proxy .mcp.json for {workdir} — \
                     the agent JWT has no parseable `sub` (agent_id) claim."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "agent JWT missing a parseable `sub` (agent_id) — proxy config NOT written",
                );
                return;
            }
        };
        let exp = jwt_unverified_claim(jwt, "exp")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let port = match bound_port {
            Some(p) => p,
            None => {
                warn!(
                    "coord_mcp: refusing to write an AGENT proxy .mcp.json for {workdir} — \
                     the bound API port is unresolvable (no managed AppState)."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "bound API port unresolvable — agent proxy config NOT written (would point at a dead port)",
                );
                return;
            }
        };
        let slot = std::sync::Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
            token: jwt.to_string(),
            jti: Uuid::nil(),
            exp,
        }));
        register_agent_token(agent_id, slot);
        write_coord_mcp_agent_proxy_config(workdir, port, agent_id);
        probe_and_breadcrumb_proxy(workdir, port);
    }
}

/// Decode an unverified string claim from a JWT payload. We do NOT verify the
/// signature — coord re-validates the EdDSA signature on use; here we only need
/// `sub_type` to avoid writing a non-coord-verifying bearer (e.g. a Cognito
/// token) into `.mcp.json`. Mirror of `cognito::jwt_claim`, inlined because that
/// fn lives in the lib crate (`pub(crate)`) and `agent_runtime` compiles into
/// the bin crate, which does not re-declare `mod cognito`.
fn jwt_unverified_claim(token: &str, claim: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut parts = token.splitn(3, '.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get(claim).and_then(|v| v.as_str()).map(String::from)
}

/// Compare two directory paths for identity, tolerating the shapes the runner
/// actually sees: mixed `/` and `\` separators, a trailing separator, and
/// Windows' case-insensitive filesystem. Prefers `canonicalize` (resolves `..`,
/// symlinks, junctions and 8.3 short names) and falls back to a normalized
/// string compare when either path does not exist — a fallback the unit tests
/// depend on, since they compare synthetic paths.
fn same_dir(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return x == y;
    }
    fn normalize(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/');
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    }
    normalize(a) == normalize(b)
}

/// Pure core of the shared-root write guard: may a runner with this
/// `owns_shared_root_state` classification write `<workdir>/.mcp.json`?
///
/// Only ONE directory is protected — the umbrella root itself. A secondary keeps
/// full authority over every other workdir (its own session cwds, worktrees and
/// per-repo checkouts), because those configs are what make in-session recovery
/// possible at all: when root is hijacked, probing the siblings for a live one is
/// the recovery. Blanket-refusing a secondary would destroy that asset.
fn shared_root_write_allowed_at(
    workdir: &str,
    root_dir: Option<&Path>,
    owns_shared_root_state: bool,
) -> bool {
    if owns_shared_root_state {
        return true;
    }
    match root_dir {
        Some(root) => !same_dir(Path::new(workdir), root),
        // No resolvable umbrella root → there is no shared root config to
        // protect, so nothing to refuse.
        None => true,
    }
}

/// True iff writing our coord-mcp `.mcp.json` into `workdir` would not clobber a
/// user's own config: the file is absent/unreadable, OR it parses as a config
/// whose `mcpServers` is solely our `coord-mcp` entry (a prior provisioning we
/// own and may refresh). A foreign or unparseable file returns false (leave it).
///
/// **Shared-root guard (plan 2026-07-20-ephemeral-runner-hijacks-root-mcp-json).**
/// Before any of that, a SECONDARY instance is refused outright when `workdir` is
/// the umbrella root: the shared root `.mcp.json` is the primary's to own. This
/// lands here — at the chokepoint every writer funnels through — rather than only
/// at the boot self-heal, because
/// `acquire_for_terminal` → [`provision_coord_mcp_for_session`] is a SECOND,
/// independent writer that reaches the root file without ever calling
/// [`reconcile_root_config`]: an operator tab opened at `D:/qontinui-root` on a
/// temp runner rewrites root to the temp port with no boot reconcile involved.
/// Guarding only the self-heal would leave that hole wide open.
fn coord_mcp_safe_to_write(workdir: &str) -> bool {
    if !shared_root_write_allowed_at(
        workdir,
        qontinui_root_dir().as_deref(),
        crate::instance::owns_shared_root_state(),
    ) {
        warn!(
            "coord_mcp: REFUSING to write {workdir}/.mcp.json — this runner is a \
             SECONDARY instance (name={:?}, port={}) and the umbrella-root \
             .mcp.json is the PRIMARY's shared state. Writing our ephemeral port \
             + nonce there would strand every root-opened session on a dead \
             endpoint once this runner exits.",
            crate::instance::instance_name(),
            crate::mcp::types::get_mcp_api_port(),
        );
        return false;
    }

    let path = Path::new(workdir).join(".mcp.json");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return true, // absent (or unreadable) → safe to create
    };
    let parsed: serde_json::Value = match serde_json::from_str(&existing) {
        Ok(v) => v,
        Err(_) => return false, // unparseable foreign file → do not clobber
    };
    match parsed.get("mcpServers").and_then(|m| m.as_object()) {
        Some(servers) => {
            if servers.len() == 1 && servers.contains_key("coord-mcp") {
                // Our own coord-mcp config — refreshable, EXCEPT never downgrade an
                // existing agent JWT (richer scopes) to a device JWT. If the current
                // bearer decodes sub_type=agent, leave it. The device-path PROXY
                // shape (loopback URL + nonce header) has NO Authorization header,
                // so it deliberately falls through this check as ours-refreshable.
                let existing_is_agent = parsed
                    .pointer("/mcpServers/coord-mcp/headers/Authorization")
                    .and_then(|v| v.as_str())
                    .and_then(|h| h.strip_prefix("Bearer "))
                    .and_then(|tok| jwt_unverified_claim(tok, "sub_type"))
                    .map(|st| st == "agent")
                    .unwrap_or(false);
                !existing_is_agent
            } else {
                false
            }
        }
        None => false,
    }
}

/// Read the loopback proxy port out of an existing coord-mcp `.mcp.json`, if the
/// file holds the PROXY shape (`url == http://127.0.0.1:<port>/coord-mcp`).
/// Returns `None` for an absent/unparseable file or a non-proxy (static-bearer)
/// shape — the latter is the agent path, which the reconcile must never touch.
fn read_proxy_port(workdir: &str) -> Option<u16> {
    let path = Path::new(workdir).join(".mcp.json");
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let url = v
        .pointer("/mcpServers/coord-mcp/url")
        .and_then(|u| u.as_str())?;
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = rest.strip_suffix("/coord-mcp")?;
    port_str.parse::<u16>().ok()
}

/// True iff `<workdir>/.mcp.json` already declares a `coord-mcp` server (ANY
/// shape — proxy, static-bearer, or a hand-authored operator entry). The identity
/// seam uses this to SKIP injecting its app-data `--mcp-config` when the cwd
/// already provides coord-mcp, so a session never ends up with two `coord-mcp`
/// entries competing for the per-workdir nonce (the loser 401s and the client
/// marks it FAILED). Covers three real cases at once:
///   * a gate-continuation terminal, whose device `.mcp.json` is written by
///     [`provision_coord_mcp_for_session`] BEFORE the terminal spawn/seam runs;
///   * the operator's own repo-root `.mcp.json` (boot self-heal keeps it on the
///     bound proxy port);
///   * any session re-spawned into a cwd a prior provision already wrote.
/// A cwd with NO `.mcp.json`, or one whose `.mcp.json` has only the user's OWN
/// (non-coord) servers, returns `false` → the seam injects `--mcp-config`, which
/// merges additively without touching the user's file.
pub(crate) fn workdir_declares_coord_mcp(workdir: &str) -> bool {
    let path = Path::new(workdir).join(".mcp.json");
    let Ok(s) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return false;
    };
    v.pointer("/mcpServers/coord-mcp").is_some()
}

/// Stable per-workdir filename for the runner-owned coord-mcp `--mcp-config` file.
/// A non-cryptographic hash of the absolute workdir keeps the name short (Windows
/// path limits) and collision-free in practice, and STABLE across re-spawns into
/// the same cwd so a restart reuses one path (rewritten with the fresh nonce).
fn mcp_config_file_name(workdir: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut h);
    format!("coord-mcp-{:016x}.json", h.finish())
}

/// Materialize a DEVICE-scope coord-mcp `--mcp-config` file into the runner's OWN
/// app-data dir (`~/.qontinui/runner/session-restore/coord-mcp/`, NEVER the cwd)
/// and return its absolute path, so the identity shim can append
/// `--mcp-config <path>` to a claude launch that would not otherwise get coord-mcp
/// (a plain/hand-typed terminal in an arbitrary cwd, a restore-re-spawned session,
/// a fresh install). Mirrors the `--settings` hook delivery
/// ([`crate::session::claude_hook::materialize`]): a runner-owned file + an env
/// var, never touching `~/.claude` nor the user's cwd.
///
/// DEVICE principal ONLY. The identity seam ([`TerminalSession::apply_identity_seam`])
/// runs exclusively for interactive + gate-continuation terminals (device scope);
/// headless agent subprocesses launch through a separate direct-`tokio` spawn
/// (`agent_runtime::run_agent_subprocess` → `spawn_claude_child`) that never
/// reaches the seam and provisions its own AGENT proxy config in-worktree. So this
/// can never elevate an agent session onto the device JWT (the scope-elevation
/// trap [`write_coord_mcp_agent_proxy_config`] guards against).
///
/// Fail-closed (Phase 3a) on an unresolvable bound API port: returns `None` rather
/// than writing a config pointing at the dead bootstrap-default `:9876` (wrong on
/// any secondary/temp runner — the F1 root cause). The caller (the seam) then sets
/// no env var, so the shim appends nothing — fail-open to NO coord-mcp, never a
/// connection-refused server the client marks FAILED, and NO breadcrumb written
/// into the user's cwd (the pollution non-goal). The nonce is DEVICE-principal and
/// persisted ([`register_proxy_nonce`]), so an already-written file keeps
/// validating across an orphan-outliving-a-restart edge.
pub(crate) fn provision_coord_mcp_config_file(workdir: &str) -> Option<std::path::PathBuf> {
    // The shared mint core (§2): fail-closed port resolve + a DEVICE, cwd-bound
    // nonce. `Persistent` = the runner-spawn class — today's semantics exactly.
    let mcp_config = mint_device_proxy_config(workdir, NonceLifetime::Persistent)?;
    let dir = crate::session::claude_hook::session_restore_dir().join("coord-mcp");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(
            "coord_mcp: failed to create app-data mcp-config dir {}: {e} — \
             --mcp-config delivery off for {workdir} (session simply has no coord-mcp)",
            dir.display()
        );
        return None;
    }
    // Restrict the directory BEFORE writing, so the credential is never even
    // briefly world-readable inside a permissive parent.
    restrict_to_owner(&dir, true);
    let file = dir.join(mcp_config_file_name(workdir));
    match std::fs::write(
        &file,
        serde_json::to_string_pretty(&mcp_config).unwrap_or_default(),
    ) {
        Ok(()) => {
            restrict_to_owner(&file, false);
            info!(
                "coord_mcp: wrote --mcp-config file {} for workdir {workdir}",
                file.display()
            );
            // Rotation forensics (Phase 4/R6): the app-data `--mcp-config`
            // materialization carries a fresh key to identity-seam sessions
            // exactly like an in-cwd `.mcp.json` write does — give it the
            // same "write" line so those sessions get the full trail.
            let key = mcp_config
                .pointer("/mcpServers/coord-mcp/headers/X-Coord-Mcp-Proxy-Key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            log_rotation_event(
                "write",
                workdir,
                key,
                "app-data --mcp-config file materialized (proxy shape)",
            );
            Some(file)
        }
        Err(e) => {
            warn!(
                "coord_mcp: failed to write --mcp-config file {}: {e} — \
                 --mcp-config delivery off for {workdir}",
                file.display()
            );
            None
        }
    }
}

/// **The ONE mint path** (plan 2026-07-17 §2): resolve the bound port
/// fail-closed, mint a DEVICE-principal nonce bound to `workdir`, and return the
/// proxy config document. Both consumers go through here —
/// [`provision_coord_mcp_config_file`] (the identity seam, which then
/// materializes the doc as an app-data file) and
/// [`provision_session_proxy_config`] (the `/coord-mcp/provision-session` mint
/// route, which returns the doc over loopback). One mint path ⇒ ONE security
/// invariant to review, rather than a route that could drift from the seam.
///
/// The invariant, stated once: **the port is fail-closed and the nonce is DEVICE
/// + cwd-bound.** `resolve_bound_api_port()` returns `None` outside a live Tauri
/// runtime (no managed `AppState`), and this returns `None` with it rather than
/// falling back to the bootstrap-default `:9876` — which is right only by luck
/// on a single-runner box and dead on any temp runner (the F1 root cause, and
/// exactly the stale-:9879 config this plan's Phase-0 probe found in the wild).
/// The route cannot bypass this: it has no port argument to pass.
///
/// `lifetime` is the ONLY axis the two callers differ on — see [`NonceLifetime`]
/// for why the mint route's nonces are bounded and the seam's are not.
fn mint_device_proxy_config(workdir: &str, lifetime: NonceLifetime) -> Option<serde_json::Value> {
    let bound_port = resolve_bound_api_port()?;
    let nonce = if lifetime.is_ephemeral() {
        register_session_proxy_nonce(workdir)
    } else {
        register_proxy_nonce(workdir)
    };
    Some(coord_mcp_proxy_config_json(bound_port, &nonce))
}

/// Mint coord identity for a session the runner did NOT spawn: the
/// `--mcp-config` document a bare terminal's launcher can hand to `claude`
/// (plan 2026-07-17 §1). Returns the same document shape the identity seam
/// delivers, via the same [`mint_device_proxy_config`] core — the nonce is
/// DEVICE-principal, bound to `workdir`, [`NonceLifetime::Ephemeral`], and
/// never persisted.
///
/// **The caller MUST have passed [`session_identity_gate`] first.** This
/// function does not gate — it is the mint, and the route
/// ([`crate::mcp_api::coord_provision_session_handler`]) is the only caller and
/// owns the gate. Keeping the gate at the route rather than here means the
/// denial can carry an actionable HTTP status + reason instead of degrading to
/// an untyped `None` that is indistinguishable from an unresolvable port.
///
/// `None` = the bound port is unresolvable ⇒ the route must 503 rather than mint
/// a nonce paired with a port nothing is listening on.
pub(crate) fn provision_session_proxy_config(workdir: &str) -> Option<serde_json::Value> {
    mint_device_proxy_config(workdir, NonceLifetime::ephemeral())
}

/// Read the per-session proxy NONCE out of an existing coord-mcp `.mcp.json`, if
/// the file holds the PROXY shape (an `X-Coord-Mcp-Proxy-Key` header). Returns
/// `None` for an absent/unparseable file or a non-proxy shape. Used by the
/// root-config self-heal: a nonce no longer in the live registry (evicted on a
/// re-provision, or simply never restored) means the config would 401 the
/// proxy, so it must be rewritten even when the port still matches.
fn read_proxy_nonce(config_path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(config_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.pointer("/mcpServers/coord-mcp/headers/X-Coord-Mcp-Proxy-Key")
        .and_then(|n| n.as_str())
        .map(String::from)
}

/// Boot-time reconcile decision for one session's `.mcp.json` (Phase 3c). Pure
/// over its inputs (no I/O) so the rewrite predicate is unit-testable:
///
/// - `Rewrite` — the config holds the proxy shape on a port ≠ the instance's
///   current bound port → rewrite it to the correct port (+ a fresh persisted
///   nonce) so the next MCP read targets a live proxy.
/// - `Leave` — no `.mcp.json` proxy port readable, OR the port already matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconcileAction {
    Rewrite,
    Leave,
}

/// Pure reconcile predicate: given the session's currently-written proxy port
/// (if any) and the instance's current bound port, decide whether to rewrite.
pub(crate) fn reconcile_action(
    current_proxy_port: Option<u16>,
    bound_port: u16,
) -> ReconcileAction {
    match current_proxy_port {
        Some(p) if p != bound_port => ReconcileAction::Rewrite,
        _ => ReconcileAction::Leave,
    }
}

/// Boot-time self-heal decision for the ROOT `.mcp.json` (plan
/// 2026-07-07-coord-mcp-nonce-survives-runner-restart, Change 1). Finer-grained
/// than [`ReconcileAction`] because the root path can heal WITHOUT rewriting the
/// file — which is the whole point: a live MCP client caches the nonce from
/// `.mcp.json` at connect and NEVER re-reads it, so any file rewrite (even a
/// byte-different one on the same port) strands that client on a nonce the new
/// registry evicted. Adopting the on-disk nonce instead keeps the file
/// byte-identical, so the client's cached nonce keeps validating across a
/// restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootReconcileAction {
    /// Healthy (port matches AND the on-disk nonce is currently registered), or
    /// not our config to touch (absent / static-bearer shape) — do nothing.
    Leave,
    /// Adopt the exact on-disk nonce into the live registry as this workdir's
    /// Device binding — NO file rewrite. Produced only when the proxy port still
    /// matches, a non-empty nonce IS readable from disk, but that nonce is not in
    /// the live registry (evicted on a re-provision, or never restored on boot).
    AdoptNonce,
    /// Mint a fresh nonce and rewrite the file to the current bound port.
    /// Produced when the port moved (a live client must reconnect regardless, so
    /// preserving the old nonce buys nothing) OR no nonce is readable from disk.
    ///
    /// NOTE the doc-comment reasoning above — *"a live client must reconnect
    /// regardless"* — is sound for the instance's OWN config and wrong for the
    /// SHARED root one, which is why root self-heal is gated on
    /// [`crate::instance::owns_shared_root_state`] and can yield
    /// [`RootReconcileAction::SkippedSecondary`] instead.
    Rewrite,
    /// This runner is a SECONDARY instance, so it did not evaluate — let alone
    /// write — the shared root config at all. Distinct from `Leave` (which means
    /// "looked, and there was nothing to do") so the boot summary can tell an
    /// operator *why* a stale root config was not repaired.
    SkippedSecondary,
}

/// Pure resolver for [`RootReconcileAction`] — decoupled from file I/O so the
/// adopt-vs-rewrite decision is unit-testable against explicit inputs. Inputs:
/// the proxy port currently written to the root file (`None` = absent /
/// unparseable / static-bearer shape), the nonce string readable from that file
/// (`None` = no proxy-key header), whether that nonce is currently in the live
/// registry, and the instance's bound port.
pub(crate) fn root_reconcile_action(
    current_proxy_port: Option<u16>,
    on_disk_nonce: Option<&str>,
    nonce_is_registered: bool,
    bound_port: u16,
) -> RootReconcileAction {
    let Some(port) = current_proxy_port else {
        // Not our proxy shape (absent / unparseable / static-bearer agent
        // config) — never touched here.
        return RootReconcileAction::Leave;
    };
    if port != bound_port {
        // Port moved: the client's cached URL is stale too, so it must reconnect
        // to reach the live port regardless — mint fresh + rewrite.
        return RootReconcileAction::Rewrite;
    }
    // Port matches — the only remaining staleness is the nonce.
    match on_disk_nonce {
        // A non-empty nonce is readable but not registered → adopt it so the
        // live client's cached nonce validates again without a file change.
        Some(nonce) if !nonce.is_empty() && !nonce_is_registered => RootReconcileAction::AdoptNonce,
        // A registered nonce → healthy, nothing to do.
        Some(_) if nonce_is_registered => RootReconcileAction::Leave,
        // No nonce readable (or an empty one) → nothing to adopt; mint fresh.
        _ => RootReconcileAction::Rewrite,
    }
}

/// Resolve the `qontinui-root` directory the checked-in repo-root `.mcp.json`
/// lives in. Mirror of `agent_runtime::qontinui_root_dir`, inlined so this leaf
/// module does NOT depend on `agent_runtime` (which depends back on us — the
/// cycle this module was extracted to break). `QONTINUI_ROOT` env first, then
/// the Windows `D:/qontinui-root` well-known path, then `~/qontinui-root`.
fn qontinui_root_dir() -> Option<std::path::PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = std::path::PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = std::path::PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("qontinui-root"))
        .filter(|p| p.is_dir())
}

/// Re-register an EXISTING on-disk proxy nonce string into the live registry as
/// a Device binding for `workdir`, WITHOUT minting a new nonce or rewriting the
/// file (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart, Change 1).
/// This is the restart-resilient self-heal: when the root `.mcp.json` proxy port
/// still matches but its nonce was evicted / never restored, adopting the exact
/// on-disk string keeps a live MCP client's CACHED nonce validating across the
/// restart (the client never re-reads the file, so a fresh-minted-and-rewritten
/// nonce would strand it on a 401). Evicts any prior nonce for the same workdir
/// (there should be none) and mirrors the updated set to the encrypted store so
/// the adoption itself survives the NEXT restart. DEVICE binding only — this
/// path is never reached for an agent config (a static-bearer shape has no proxy
/// URL, so [`read_proxy_port`] returns `None` and the resolver leaves it).
fn adopt_on_disk_nonce(workdir: &str, nonce: &str) {
    let (snapshot, evicted) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // Persistent class only — an adopted nonce came from a runner-written
        // `.mcp.json`, and must NOT evict a bare session's ephemeral nonce for
        // the same workdir (the class-scoping rationale in
        // `mint_and_register_nonce`).
        let mut evicted: Vec<String> = Vec::new();
        map.retain(|n, b| {
            if b.workdir == workdir && !b.lifetime.is_ephemeral() {
                evicted.push(n.clone());
                return false;
            }
            true
        });
        map.insert(
            nonce.to_string(),
            NonceBinding {
                workdir: workdir.to_string(),
                principal: ProxyPrincipal::Device,
                lifetime: NonceLifetime::Persistent,
                // An adopted on-disk nonce carries no tenant (the `.mcp.json`
                // stores only URL + nonce), and its original session's tenant
                // is unrecoverable across the restart — fall back to the legacy
                // default slot (`device_bearer_for(None)`), the pre-B3 behavior
                // for these device nonces (never cross-tenant).
                session_tenant: None,
            },
        );
        (map.clone(), evicted)
    };
    // Rotation forensics — outside the lock (see `mint_and_register_nonce`).
    for n in &evicted {
        log_rotation_event(
            "evict",
            workdir,
            n,
            "superseded by on-disk nonce adoption (not graced)",
        );
    }
    log_rotation_event(
        "adopt",
        workdir,
        nonce,
        "re-registered the on-disk `.mcp.json` nonce (no file rewrite)",
    );
    persist_proxy_nonces(&snapshot);
}

/// Read the root `.mcp.json` ONCE and resolve both the self-heal action and the
/// nonce string readable from it (the [`RootReconcileAction::AdoptNonce`] arm
/// needs that string to re-register). Keeps the file read out of the pure
/// [`root_reconcile_action`] resolver while giving both callers
/// ([`root_config_is_stale`] and [`reconcile_root_config_at`]) a single,
/// consistent read.
fn resolve_root_reconcile(
    root_dir: &Path,
    bound_port: u16,
) -> (RootReconcileAction, Option<String>) {
    let path = root_dir.join(".mcp.json");
    let current_port = read_proxy_port(&root_dir.to_string_lossy());
    let on_disk_nonce = read_proxy_nonce(&path);
    let registered = on_disk_nonce
        .as_deref()
        .map(proxy_nonce_is_valid)
        .unwrap_or(false);
    let action = root_reconcile_action(
        current_port,
        on_disk_nonce.as_deref(),
        registered,
        bound_port,
    );
    (action, on_disk_nonce)
}

/// True iff the root `.mcp.json` at `root_dir` needs SOME self-heal action
/// (adopt-nonce or rewrite) for the current `bound_port`. Returns `false` (leave
/// it) for an absent file, a non-proxy (static-bearer) shape, or a proxy config
/// whose port matches AND whose nonce is currently registered. Delegates to the
/// single-read [`resolve_root_reconcile`] so the "is anything to do" predicate
/// and the "what to do" dispatch never diverge. Test-only convenience — the prod
/// dispatch reads the action directly from [`resolve_root_reconcile`].
#[cfg(test)]
fn root_config_is_stale(root_dir: &Path, bound_port: u16) -> bool {
    resolve_root_reconcile(root_dir, bound_port).0 != RootReconcileAction::Leave
}

/// Boot-time self-heal of the CHECKED-IN repo-root `.mcp.json` (Phase 5b),
/// resolving the root dir from the environment. Delegates to
/// [`reconcile_root_config_at`] — split so the heal is unit-testable against
/// an explicit temp dir WITHOUT mutating the process-global `QONTINUI_ROOT`
/// (the module deliberately avoids global-env mutation; see the test helpers).
///
/// `pub(crate)` so the boot task can call root self-heal UNCONDITIONALLY —
/// independent of whether any live session workdir is present (plan
/// 2026-07-07 Change 1 secondary gap: a boot with zero open sessions must still
/// repair a stale-port root config, which the old session-gated wiring skipped).
/// Returns the [`RootReconcileAction`] actually taken (`Leave` when there is no
/// resolvable root dir, `SkippedSecondary` on a non-canonical instance) so the
/// boot task can log the restore-vs-heal outcome.
///
/// **Instance-gated (plan 2026-07-20-ephemeral-runner-hijacks-root-mcp-json).**
/// "Unconditional" was the right fix for the original bug (a boot with zero open
/// sessions used to skip root repair entirely) but it was never gated on WHICH
/// instance is booting, and the action is chosen purely by port comparison — so
/// any ephemeral runner on `:9877` saw `9876 != 9877`, took the `Rewrite` arm,
/// and claimed the shared root config for a process about to exit. The heal stays
/// unconditional with respect to session presence; it is now conditional on being
/// the instance that OWNS that file.
pub(crate) fn reconcile_root_config(bound_port: u16) -> RootReconcileAction {
    reconcile_root_config_gated(
        qontinui_root_dir().as_deref(),
        bound_port,
        crate::instance::owns_shared_root_state(),
    )
}

/// Env-free, instance-gated core of [`reconcile_root_config`] — split out so the
/// guard is unit-testable against an explicit temp dir and an injected
/// classification, without mutating process-global env (which races the parallel
/// test harness).
///
/// The instance check runs FIRST, ahead of the root-dir match, so a secondary
/// reports [`RootReconcileAction::SkippedSecondary`] uniformly — including when
/// no root dir resolves. Ordering the two the other way would report `Leave` for
/// that case and lose the "a secondary declined" signal precisely where an
/// operator is already confused about why nothing was repaired. The shared root
/// config is not merely left unwritten but never opened.
///
/// Prevention, not restoration: the tempting alternative — let the secondary
/// overwrite root and restore it on exit — is rejected, because a SIGKILL, panic
/// or runtime teardown skips exit hooks, converting a deterministic bug into an
/// intermittent one. A file that is never wrongly written needs no restoration.
///
/// Accepted consequence: when no primary is running, nobody heals a stale root
/// config. That is correct — a root config naming a dead PRIMARY self-corrects
/// the moment the primary boots, whereas one naming a dead EPHEMERAL runner is
/// indistinguishable from a live config until the request fails.
fn reconcile_root_config_gated(
    root_dir: Option<&Path>,
    bound_port: u16,
    owns_shared_root_state: bool,
) -> RootReconcileAction {
    if !owns_shared_root_state {
        info!(
            "coord_mcp: root self-heal SKIPPED — this runner is a SECONDARY \
             instance (name={:?}, port={}); the shared root .mcp.json at {:?} \
             belongs to the primary and is left untouched",
            crate::instance::instance_name(),
            crate::mcp::types::get_mcp_api_port(),
            root_dir,
        );
        return RootReconcileAction::SkippedSecondary;
    }
    match root_dir {
        Some(dir) => reconcile_root_config_at(dir, bound_port),
        None => RootReconcileAction::Leave,
    }
}

/// Self-heal the repo-root `.mcp.json` under `root_dir` (Phase 5b + plan
/// 2026-07-07 Change 1). The root config is the loopback PROXY shape (port +
/// per-session nonce); a spawned agent that inherits it (cwd up the tree, no
/// per-worktree config) — and a long-running operator session whose MCP client
/// cached the nonce — break if the nonce was evicted or the port moved across a
/// restart/instance change. Dispatches on [`resolve_root_reconcile`]:
///
/// - `AdoptNonce` — port unchanged, a nonce IS on disk but unregistered:
///   re-register that EXACT string ([`adopt_on_disk_nonce`]), leaving the file
///   byte-identical so a live client's cached nonce keeps validating. Returns
///   `false` (no file rewrite).
/// - `Rewrite` — port moved (client must reconnect regardless) or no nonce
///   readable: mint fresh + rewrite via [`write_coord_mcp_proxy_config`].
///   Returns `true`.
/// - `Leave` — healthy or not ours. Returns `false`.
///
/// Both mutating arms are guarded by [`coord_mcp_safe_to_write`] so a
/// hand-rolled static-bearer root file is never clobbered. Returns the
/// [`RootReconcileAction`] actually applied — a guard refusal downgrades to
/// `Leave` (nothing happened), so the return is an honest record of the effect.
fn reconcile_root_config_at(root_dir: &Path, bound_port: u16) -> RootReconcileAction {
    let (action, on_disk_nonce) = resolve_root_reconcile(root_dir, bound_port);
    let root = root_dir.to_string_lossy().to_string();
    match action {
        RootReconcileAction::Leave => RootReconcileAction::Leave,
        RootReconcileAction::AdoptNonce => {
            if !coord_mcp_safe_to_write(&root) {
                return RootReconcileAction::Leave;
            }
            // SAFETY: the resolver only yields AdoptNonce when a non-empty nonce
            // was read from disk, so this Option is always Some here.
            let nonce = on_disk_nonce
                .expect("AdoptNonce implies a readable on-disk nonce (resolver invariant)");
            adopt_on_disk_nonce(&root, &nonce);
            info!(
                "coord_mcp: boot self-heal ADOPTED on-disk root nonce for {root} \
                 (port :{bound_port} unchanged; .mcp.json byte-identical) — live \
                 MCP client cache preserved"
            );
            RootReconcileAction::AdoptNonce
        }
        RootReconcileAction::Rewrite => {
            if !coord_mcp_safe_to_write(&root) {
                return RootReconcileAction::Leave;
            }
            write_coord_mcp_proxy_config(&root, bound_port);
            info!(
                "coord_mcp: boot self-heal rewrote root {root}/.mcp.json to bound \
                 port :{bound_port} (fresh nonce — port moved or no on-disk nonce)"
            );
            RootReconcileAction::Rewrite
        }
        // Unreachable in practice: the pure resolver ([`root_reconcile_action`])
        // only ever yields Leave/AdoptNonce/Rewrite — `SkippedSecondary` is
        // produced solely by the instance gate in [`reconcile_root_config_gated`],
        // which returns BEFORE calling this function. Handle it as identity rather
        // than `unreachable!` so a future resolver change degrades to a no-op skip
        // instead of a panic on the boot path.
        RootReconcileAction::SkippedSecondary => RootReconcileAction::SkippedSecondary,
    }
}

/// Boot-time session-config reconcile (Phase 3c). For each live session workdir,
/// if its `.mcp.json` coord-mcp proxy port ≠ the instance's CURRENT bound port,
/// rewrite it via [`write_coord_mcp_proxy_config`] (correct port + a freshly
/// persisted nonce), guarded by [`coord_mcp_safe_to_write`] so it never clobbers
/// an agent-spawn's static-bearer config. Combined with Phase 3b (persisted
/// nonces), the common same-port restart needs no rewrite at all — this covers
/// only the instance/port-change case. Returns the number of SESSION configs
/// rewritten.
///
/// Root-config self-heal is NOT done here — the boot task calls
/// [`reconcile_root_config`] UNCONDITIONALLY (plan 2026-07-07 Change 1 secondary
/// gap: a boot with zero open sessions must still repair the root config, which
/// the old session-gated tail-call skipped). Keeping the two concerns separate
/// lets the caller heal the root even when this fn is not called at all.
///
/// Wired into the same startup path as the other auto-start tasks (see
/// `mcp_api::start_server`), AFTER [`restore_proxy_nonces_from_store`] so a
/// rewrite reuses the restored map where possible.
pub(crate) fn reconcile_session_configs<I>(workdirs: I, bound_port: u16) -> usize
where
    I: IntoIterator<Item = String>,
{
    let mut rewritten = 0usize;
    for workdir in workdirs {
        if reconcile_action(read_proxy_port(&workdir), bound_port) != ReconcileAction::Rewrite {
            continue;
        }
        if !coord_mcp_safe_to_write(&workdir) {
            // An agent-spawn static-bearer config (or a user's own file) — never
            // clobber. (A proxy-shaped config is ours-refreshable, so this only
            // skips configs we must not touch.)
            continue;
        }
        write_coord_mcp_proxy_config(&workdir, bound_port);
        rewritten += 1;
        info!("coord_mcp: reconciled {workdir}/.mcp.json to bound port :{bound_port}");
    }
    if rewritten > 0 {
        info!(
            "coord_mcp: boot reconcile rewrote {rewritten} session config(s) to the current bound port"
        );
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_env::env_lock;

    /// `workdir_declares_coord_mcp` — the identity-seam skip gate — is true iff
    /// `<workdir>/.mcp.json` declares a `coord-mcp` server (any shape), false for
    /// absent / unparseable / non-coord `.mcp.json`.
    #[test]
    fn workdir_declares_coord_mcp_detects_any_coord_entry() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-declares-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        // Absent → false (seam injects).
        assert!(!workdir_declares_coord_mcp(&wd));

        // A user's OWN non-coord servers → false (seam injects --mcp-config,
        // merging additively without touching their file).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"my-server":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();
        assert!(!workdir_declares_coord_mcp(&wd));

        // A coord-mcp proxy entry → true (seam skips — continuation terminal /
        // operator root / previously-provisioned cwd already provides it).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp"}}}"#,
        )
        .unwrap();
        assert!(workdir_declares_coord_mcp(&wd));

        // Unparseable → false (never a false-skip on a garbage file).
        std::fs::write(&mcp, "not json {").unwrap();
        assert!(!workdir_declares_coord_mcp(&wd));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Session-provisioned coord identity (plan 2026-07-17 §1/§2/§3)
    // -----------------------------------------------------------------------

    /// The route's authorization gate — the substitute for the nonce check every
    /// sibling `/coord-mcp/*` route has. BOTH gates are required, and the
    /// DEFAULT (flag unset) is denied: the feature ships dark, so an un-flagged
    /// runner exposes nothing. Pure resolver ⇒ no env/home-dir mutation.
    #[test]
    fn session_identity_gate_requires_flag_and_marker_and_defaults_denied() {
        // Default posture: flag off ⇒ FlagOff, regardless of the marker. This is
        // the "flag OFF ⇒ zero behavior change" acceptance criterion.
        assert_eq!(
            resolve_session_identity_gate(false, false),
            Err(SessionIdentityDenial::FlagOff)
        );
        assert_eq!(
            resolve_session_identity_gate(false, true),
            Err(SessionIdentityDenial::FlagOff),
            "an opted-in machine must STILL be denied while the master flag is dark"
        );
        // Flag on but not opted in ⇒ a DISTINCT reason (different fix).
        assert_eq!(
            resolve_session_identity_gate(true, false),
            Err(SessionIdentityDenial::NotOptedIn)
        );
        // Both ⇒ allowed.
        assert_eq!(resolve_session_identity_gate(true, true), Ok(()));

        // The two denials never collapse into one code — a caller must be able
        // to tell "the feature is dark" from "this machine has not opted in".
        assert_ne!(
            SessionIdentityDenial::FlagOff.code(),
            SessionIdentityDenial::NotOptedIn.code()
        );
        // ...and the not-opted-in message names the marker path to create.
        assert!(SessionIdentityDenial::NotOptedIn
            .message()
            .contains(SESSION_IDENTITY_MARKER_FILE));
    }

    /// The live process gate: with the master flag unset (the default in a test
    /// process, and in production), `session_identity_gate` denies with
    /// `FlagOff` WITHOUT touching the filesystem for the marker.
    #[test]
    fn session_identity_gate_is_dark_by_default_in_this_process() {
        assert_eq!(
            session_identity_gate(),
            Err(SessionIdentityDenial::FlagOff),
            "the master flag is unset by default ⇒ the mint route is dark"
        );
    }

    /// §1/E — the mint route's nonces are EPHEMERAL: revoked the moment the
    /// machine is opted out (the operator's real off switch, re-checked per
    /// request rather than only at mint), while a runner-spawn PERSISTENT nonce
    /// for the SAME workdir is completely unaffected.
    ///
    /// This is the "a runner-spawned terminal must be completely unaffected"
    /// invariant, in its sharpest form: the two classes coexist for one workdir,
    /// and neither can evict or revoke the other.
    #[test]
    fn ephemeral_nonce_is_revoked_by_the_gate_while_persistent_is_untouched() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-eph-{}", uuid::Uuid::now_v7()));
        let wd = dir.to_string_lossy().to_string();

        // A live PTY terminal's nonce for this cwd (runner-spawn class).
        let pty_nonce = register_proxy_nonce(&wd);
        // A bare session mints for the SAME cwd (mint-route class).
        let bare_nonce = register_session_proxy_nonce(&wd);
        assert_ne!(pty_nonce, bare_nonce);

        // The bare mint did NOT evict the PTY nonce — an unprivileged mint-route
        // call naming the operator's repo root must never 401 a live terminal.
        assert!(
            proxy_nonce_is_valid(&pty_nonce),
            "an ephemeral mint for a workdir must not evict that workdir's PTY nonce"
        );
        assert_eq!(workdir_for_nonce(&pty_nonce).as_deref(), Some(wd.as_str()));

        // The master flag is off in this test process ⇒ the gate denies ⇒ the
        // ephemeral nonce is revoked live, while the persistent one is not.
        assert!(
            !proxy_nonce_is_valid(&bare_nonce),
            "an ephemeral nonce must stop validating while the machine is opted out"
        );
        assert_eq!(
            proxy_principal_for_nonce(&bare_nonce),
            None,
            "a revoked ephemeral nonce resolves to no principal (the proxy 401s)"
        );
        assert!(
            proxy_nonce_is_valid(&pty_nonce),
            "revocation of the mint-route class must never touch the runner-spawn class"
        );

        // Both mint DEVICE principals — the route can never elevate to agent.
        let bare_again = register_session_proxy_nonce(&wd);
        assert!(matches!(
            proxy_nonces()
                .lock()
                .unwrap()
                .get(&bare_again)
                .map(|b| b.principal.clone()),
            Some(ProxyPrincipal::Device)
        ));
        // An ephemeral mint evicts NOTHING: two DIFFERENT bare sessions routinely
        // share a cwd, and ephemeral evictions are not graced — so a second
        // ephemeral mint for this workdir must leave the FIRST one registered, or
        // the first session's already-connected MCP client would 401 mid-session.
        // Both bindings coexist, each living out its own TTL.
        {
            let map = proxy_nonces().lock().unwrap();
            assert!(
                map.contains_key(&bare_nonce),
                "an ephemeral mint must NOT evict a prior same-workdir ephemeral nonce"
            );
            assert!(
                map.contains_key(&bare_again),
                "the freshly-minted ephemeral nonce is registered alongside it"
            );
        }
    }

    /// Bound the map: an EXPIRED ephemeral binding is swept from the live map on
    /// the next mint (whatever the new mint's workdir/class), so a long-lived
    /// opted-in runner minting across many distinct cwds cannot leak unbounded
    /// ephemeral bindings. A live (unexpired) ephemeral and a persistent binding
    /// both survive the sweep.
    #[test]
    fn expired_ephemeral_nonces_are_swept_on_mint() {
        let wd = format!("D:/sweep-test/{}", uuid::Uuid::now_v7());

        // Seed one already-expired ephemeral and one live ephemeral, both for a
        // DIFFERENT workdir than the mint below (so the sweep, not eviction, is
        // what removes the expired one).
        let expired = format!("expired-sweep-{}", uuid::Uuid::now_v7().simple());
        let live = format!("live-sweep-{}", uuid::Uuid::now_v7().simple());
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.insert(
                expired.clone(),
                NonceBinding {
                    workdir: format!("{wd}/other-expired"),
                    principal: ProxyPrincipal::Device,
                    lifetime: NonceLifetime::Ephemeral {
                        expires_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                    },
                    session_tenant: None,
                },
            );
            map.insert(
                live.clone(),
                NonceBinding {
                    workdir: format!("{wd}/other-live"),
                    principal: ProxyPrincipal::Device,
                    lifetime: NonceLifetime::Ephemeral {
                        expires_at: std::time::Instant::now()
                            + std::time::Duration::from_secs(3600),
                    },
                    session_tenant: None,
                },
            );
        }

        // Any mint triggers the opportunistic sweep.
        let persistent = register_proxy_nonce(&wd);

        let map = proxy_nonces().lock().unwrap();
        assert!(
            !map.contains_key(&expired),
            "an expired ephemeral binding is swept from the map on the next mint"
        );
        assert!(
            map.contains_key(&live),
            "a live (unexpired) ephemeral binding survives the sweep"
        );
        assert!(
            map.contains_key(&persistent),
            "the freshly-minted persistent nonce is registered"
        );
    }

    /// §1/E — an ephemeral nonce fails closed at its deadline and is lazily
    /// evicted (so the map stays bounded), while a persistent nonce has no
    /// expiry at all. Drives the expiry through `live_binding` directly rather
    /// than waiting out [`EPHEMERAL_NONCE_TTL`].
    #[test]
    fn ephemeral_nonce_expires_and_evicts_while_persistent_never_expires() {
        let wd = format!("D:/expiry-test/{}", uuid::Uuid::now_v7());

        // An already-expired ephemeral binding.
        let expired = "expired-nonce-for-lifetime-test".to_string();
        proxy_nonces().lock().unwrap().insert(
            expired.clone(),
            NonceBinding {
                workdir: wd.clone(),
                principal: ProxyPrincipal::Device,
                lifetime: NonceLifetime::Ephemeral {
                    expires_at: std::time::Instant::now() - std::time::Duration::from_secs(1),
                },
                session_tenant: None,
            },
        );
        assert!(
            !proxy_nonce_is_valid(&expired),
            "an ephemeral nonce past its deadline fails closed"
        );
        assert!(
            !proxy_nonces().lock().unwrap().contains_key(&expired),
            "an expired ephemeral nonce is lazily evicted so the map stays bounded"
        );

        // A persistent nonce is valid indefinitely — no clock involved. This is
        // the property nonce persistence + the restart grace window depend on
        // (the MCP client never re-reads its config), and the reason the TTL is
        // scoped to the mint route instead of applied globally.
        let persistent = register_proxy_nonce(&wd);
        assert!(proxy_nonce_is_valid(&persistent));
    }

    /// §1/E — an ephemeral nonce NEVER reaches disk. The store has no expiry
    /// column, so a persisted ephemeral nonce would restore as an UNBOUNDED
    /// one — laundering the weaker class into the stronger one across a restart.
    /// The runner-spawn nonce in the same snapshot still persists.
    #[test]
    fn ephemeral_nonces_are_never_persisted() {
        let (dir, store) = temp_store("ephemeral-never-persisted");
        let wd = format!("D:/persist-test/{}", uuid::Uuid::now_v7());

        let persistent = register_proxy_nonce(&wd);
        let ephemeral = register_session_proxy_nonce(&wd);

        let snapshot = proxy_nonces().lock().unwrap().clone();
        persist_proxy_nonces_with_store(&store, &snapshot);

        let loaded = store.load_coord_mcp_nonces();
        assert!(
            loaded.contains_key(&persistent),
            "a runner-spawn device nonce still persists (restart survival is its whole point)"
        );
        assert!(
            !loaded.contains_key(&ephemeral),
            "a mint-route nonce must never reach disk — it would restore unbounded"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §2 — the seam and the mint route emit the SAME document shape from the
    /// same writer: loopback URL on the passed bound port, the nonce header, and
    /// NO baked bearer. One writer means the bare-session path can never drift
    /// from the runner-spawn path.
    #[test]
    fn proxy_config_json_is_one_shape_for_both_mint_paths() {
        let v = coord_mcp_proxy_config_json(9877, "abc123");
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:9877/coord-mcp");
        assert_eq!(server["headers"]["X-Coord-Mcp-Proxy-Key"], "abc123");
        assert!(
            server["headers"].get("Authorization").is_none(),
            "the proxy shape must never bake a static bearer"
        );
    }

    /// §2 — the mint route inherits the seam's fail-closed port check because it
    /// shares the mint core: with no Tauri runtime / managed AppState,
    /// `resolve_bound_api_port` is `None`, so the route mints NOTHING (and its
    /// handler 503s) rather than pairing a nonce with a bootstrap-default port
    /// that is dead on any secondary/temp runner.
    #[test]
    fn provision_session_proxy_config_fail_closed_without_bound_port() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-sessprov-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();

        assert!(
            provision_session_proxy_config(&wd).is_none(),
            "no bound port ⇒ the mint route refuses to mint (fail-closed, shared with the seam)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every config this module writes carries a replayable proxy nonce, so it
    /// must not be world-readable. Regression for the 2026-07-21 finding: a
    /// config minted 2026-07-13 was still `-rw-r--r--` and still valid, so any
    /// local process could read it and act as this device.
    ///
    /// The `unix` arm asserts the mode exactly. On Windows the DACL is set via
    /// `icacls`, whose result is not readable through `std::fs::Permissions` —
    /// so there the regression that actually matters is **self-lockout**: after
    /// restricting, we must still be able to read our own credential back.
    #[test]
    fn restrict_to_owner_makes_a_credential_file_owner_only() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-acl-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("coord-mcp-test.json");
        std::fs::write(&file, r#"{"nonce":"secret"}"#).unwrap();

        restrict_to_owner(&file, false);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "credential file must be owner-only, got {mode:o}"
            );
        }

        // Both platforms: the owner must not lock themselves out — a restriction
        // that breaks our own read would break every session spawn.
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            r#"{"nonce":"secret"}"#,
            "restricting must not cost the owner their own read"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Best-effort contract: restricting a path that does not exist must not
    /// panic or propagate — the callers treat hardening as advisory and keep
    /// delivering coord-mcp regardless.
    #[test]
    fn restrict_to_owner_is_best_effort_on_a_missing_path() {
        let missing =
            std::env::temp_dir().join(format!("coord-mcp-absent-{}", uuid::Uuid::now_v7()));
        restrict_to_owner(&missing, false);
        restrict_to_owner(&missing, true);
    }

    /// Phase 3a fail-closed: with no Tauri runtime / managed AppState in a unit
    /// test, `resolve_bound_api_port` returns `None`, so `provision_coord_mcp_config_file`
    /// writes NOTHING and returns `None` — the seam then injects no
    /// QONTINUI_MCP_CONFIG (fail-open to no coord-mcp, never a dead-port config,
    /// and — critically — no breadcrumb written into the user's cwd).
    #[test]
    fn provision_coord_mcp_config_file_fail_closed_without_bound_port() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-provfile-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();

        assert!(
            provision_coord_mcp_config_file(&wd).is_none(),
            "no bound port ⇒ no --mcp-config file (fail-closed)"
        );
        // And no cwd pollution — the degraded breadcrumb belongs to the workdir
        // `.mcp.json` path, never the seam's arbitrary-cwd delivery.
        assert!(
            !dir.join(COORD_MCP_STATUS_FILE).exists(),
            "seam fail-closed must not pollute the cwd with a breadcrumb"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The per-workdir `--mcp-config` filename is stable for a given workdir and
    /// distinct across workdirs (so two cwds never collide on one app-data file).
    #[test]
    fn mcp_config_file_name_is_stable_and_workdir_distinct() {
        let a1 = mcp_config_file_name("D:/repo/one");
        let a2 = mcp_config_file_name("D:/repo/one");
        let b = mcp_config_file_name("D:/repo/two");
        assert_eq!(a1, a2, "stable across calls for one workdir");
        assert_ne!(a1, b, "distinct across workdirs");
        assert!(a1.starts_with("coord-mcp-") && a1.ends_with(".json"));
    }

    /// A temp-dir-backed [`SecureStorage`] for the persistence tests — injected
    /// directly into the `_with_store` seam so a test NEVER mutates the
    /// process-global `QONTINUI_SECURE_STORAGE_DIR`. Mutating that env var raced
    /// sibling tests that read the DEFAULT store (notably
    /// `auth::device_jwt_tests::needs_refresh_when_no_token`), which is why this
    /// module no longer touches global env at all.
    fn temp_store(tag: &str) -> (std::path::PathBuf, crate::secure_storage::SecureStorage) {
        let dir =
            std::env::temp_dir().join(format!("coord-mcp-{tag}-store-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::secure_storage::SecureStorage::with_path(dir.join("nonces.enc"))
            .expect("temp secure storage");
        (dir, store)
    }

    /// The DEVICE-path proxy shape: loopback URL built from the passed bound
    /// port, a registered per-session nonce header, and — critically — NO
    /// baked `Authorization` bearer (the proxy injects a live one per request).
    #[test]
    fn write_coord_mcp_proxy_config_emits_loopback_nonce_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-proxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let primary_wt = tmp.to_string_lossy().to_string();

        write_coord_mcp_proxy_config(&primary_wt, 23456);

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];

        assert_eq!(server["type"], "http");
        assert_eq!(
            server["url"], "http://127.0.0.1:23456/coord-mcp",
            "URL must target the loopback proxy on the PASSED bound port"
        );

        // The nonce header is present, non-empty, and live in the registry.
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("proxy config must carry the per-session nonce header");
        assert!(!nonce.is_empty());
        assert!(
            proxy_nonce_is_valid(nonce),
            "the written nonce must be registered for the proxy gate"
        );

        // NO baked bearer — the whole point is the proxy injects a live one.
        assert!(
            server["headers"].get("Authorization").is_none(),
            "proxy shape must NOT bake a static Authorization bearer: {written}"
        );

        // Re-provisioning the same workdir mints a FRESH nonce and moves the
        // prior one onto the grace TTL (plan 2026-07-07 Change 3): the old nonce
        // is dropped from the LIVE map but stays valid briefly so an in-flight
        // client that cached it rides through until it reconnects — rather than
        // hard-401ing the instant `.mcp.json` is rewritten. Both nonces resolve
        // to the same Device principal, so there is no scope-elevation surface.
        write_coord_mcp_proxy_config(&primary_wt, 23456);
        let reprovisioned = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&reprovisioned).unwrap();
        let new_nonce = v2["mcpServers"]["coord-mcp"]["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("re-provision must carry a nonce header");
        assert_ne!(new_nonce, nonce, "a re-provision must mint a fresh nonce");
        assert!(
            proxy_nonce_is_valid(new_nonce),
            "the freshly-minted nonce is live in the registry"
        );
        assert!(
            proxy_nonce_is_valid(nonce),
            "the prior device nonce rides the grace TTL (Change 3) — not hard-evicted"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The loopback proxy's pre-forward gate over a DEVICE-bound nonce:
    /// registered nonce + device bearer → forward; everything else → 401 before
    /// any network I/O. This is the scope-elevation backstop — a device nonce
    /// must never forward a non-device bearer.
    #[test]
    fn proxy_request_gate_forwards_only_nonce_plus_device_bearer() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        let dir = std::env::temp_dir().join(format!("coord-mcp-gate-{}", uuid::Uuid::new_v4()));
        let nonce = register_proxy_nonce(&dir.to_string_lossy());
        let device = mk("device");
        let dev_p = ProxyPrincipal::Device;

        // The nonce resolves to a Device principal.
        assert_eq!(
            proxy_principal_for_nonce(&nonce),
            Some(ProxyPrincipal::Device)
        );

        // Registered nonce + device bearer → forward.
        assert!(proxy_request_gate(Some(&nonce), Some(&device), &dev_p).is_ok());

        // Absent / mismatched nonce → 401, regardless of bearer.
        assert_eq!(
            proxy_request_gate(None, Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some("not-a-registered-nonce"), Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some(""), Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );

        // Valid nonce but no/empty bearer → 401.
        assert_eq!(
            proxy_request_gate(Some(&nonce), None, &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(" "), &dev_p)
                .unwrap_err()
                .0,
            401
        );

        // Valid device nonce but an AGENT bearer → 401 (scope-elevation trap:
        // a device nonce must only ever attach the runner's DEVICE identity).
        let agent_err = proxy_request_gate(Some(&nonce), Some(&mk("agent")), &dev_p).unwrap_err();
        assert_eq!(agent_err.0, 401);
        // A non-coord (e.g. Cognito) bearer → 401 too.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(&mk("access")), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        // A non-JWT string → 401, never a panic.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some("not-a-jwt"), &dev_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// The AGENT-bound nonce arm of the gate + the cross-binding rejections in
    /// BOTH directions (the structural scope-elevation backstop):
    ///  - an agent nonce resolves to `ProxyPrincipal::Agent` and forwards an
    ///    agent bearer, but REJECTS a device bearer;
    ///  - a device nonce REJECTS an agent bearer (covered above too, repeated
    ///    here for symmetry).
    #[test]
    fn proxy_request_gate_binds_agent_nonce_to_agent_bearer() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        let agent_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("coord-mcp-agate-{}", uuid::Uuid::new_v4()));
        let agent_nonce = register_agent_proxy_nonce(&dir.to_string_lossy(), agent_id);

        // The nonce resolves to an Agent principal carrying the agent_id.
        assert_eq!(
            proxy_principal_for_nonce(&agent_nonce),
            Some(ProxyPrincipal::Agent { agent_id })
        );

        let agent_p = ProxyPrincipal::Agent { agent_id };
        let device_p = ProxyPrincipal::Device;

        // Agent nonce + agent bearer → forward.
        assert!(proxy_request_gate(Some(&agent_nonce), Some(&mk("agent")), &agent_p).is_ok());

        // Agent nonce + DEVICE bearer → 401 (an agent nonce can never inject the
        // device token — the reverse scope-elevation direction).
        assert_eq!(
            proxy_request_gate(Some(&agent_nonce), Some(&mk("device")), &agent_p)
                .unwrap_err()
                .0,
            401
        );

        // And a device principal must reject an agent bearer.
        let dev_dir =
            std::env::temp_dir().join(format!("coord-mcp-dgate-{}", uuid::Uuid::new_v4()));
        let dev_nonce = register_proxy_nonce(&dev_dir.to_string_lossy());
        assert_eq!(
            proxy_request_gate(Some(&dev_nonce), Some(&mk("agent")), &device_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// Phase 3 invariant pin (terminal-autonomy-survives-logout): a MISSING
    /// device JWT degrades the coord-mcp PROXY path to an actionable retry —
    /// and ONLY that path. Local terminal AI session work (model reasoning, the
    /// PTY, local tools) is structurally independent of this gate, so a coord
    /// credential gap can never block local work.
    ///
    /// This asserts the gating is scoped to coord-mcp proxy requests:
    ///  - [`proxy_request_gate`] is the ONLY thing a missing device JWT affects;
    ///    it governs proxy forwarding exclusively (it takes a proxy nonce +
    ///    bearer + the bound principal and returns only forward-or-401). It is
    ///    not on, and has no handle into, any local-execution path.
    ///  - the missing-device-JWT case degrades to an ACTIONABLE, retry-shaped
    ///    error ([`device_jwt_refreshing_error`]) — not a panic, not a hang
    ///    (the wait is bounded, see `await_remint_*` tests), not a bare 401.
    ///  - the gate's hard nonce/scope 401s stay FAIL-CLOSED and unchanged.
    ///
    /// Why local work is independent (documented here as the layer can't call a
    /// PTY): the terminal AI session's MODEL auth is the operator's own Claude
    /// subscription via `CLAUDE_CONFIG_DIR`, and the PTY + local tools never
    /// issue a coord-mcp proxy request — only an explicit coord MCP tool call
    /// routes through this gate. So a missing device JWT can at most make a
    /// coord tool call retry; it cannot stall the session's local progress.
    #[test]
    fn missing_device_jwt_degrades_proxy_only_not_local_work() {
        // (a) The degrade is an ACTIONABLE retry, distinct from the hard 401.
        let (status, msg) = device_jwt_refreshing_error();
        assert_eq!(status, 503, "transient credential gap → retryable, not 401");
        assert!(
            msg.to_lowercase().contains("retry"),
            "degrade message must tell the caller to retry: {msg:?}"
        );
        assert!(
            !msg.contains("no live JWT available"),
            "must NOT be the bare missing-JWT 401 message"
        );

        // (b) The gate is scoped to proxy requests only: its hard nonce/scope
        // 401s remain fail-closed and unchanged. A valid device nonce with NO
        // bearer is still a hard 401 backstop (the proxy handler only reaches
        // the gate AFTER the bounded re-mint produced a usable bearer).
        let dir = std::env::temp_dir().join(format!("coord-mcp-p3-{}", uuid::Uuid::new_v4()));
        let nonce = register_proxy_nonce(&dir.to_string_lossy());
        let dev_p = ProxyPrincipal::Device;
        assert_eq!(
            proxy_request_gate(Some(&nonce), None, &dev_p)
                .unwrap_err()
                .0,
            401,
            "missing bearer at the gate is still a hard 401 backstop"
        );
        // Bad nonce stays a hard 401 regardless of the degrade path.
        assert_eq!(
            proxy_request_gate(Some("nope"), None, &dev_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// The bounded re-mint wait TERMINATES on the deadline (never hangs) when no
    /// usable JWT ever appears — pins "NEVER block a request indefinitely".
    #[tokio::test]
    async fn await_remint_is_bounded_when_no_jwt_appears() {
        let calls = std::cell::Cell::new(0u32);
        let started = std::time::Instant::now();
        let out = await_remint_with(
            || {
                calls.set(calls.get() + 1);
                async { None }
            },
            std::time::Duration::from_millis(120),
            std::time::Duration::from_millis(20),
        )
        .await;
        assert_eq!(out, None, "no JWT → None after the bound");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "must return promptly at the bound, not hang"
        );
        assert!(calls.get() >= 2, "should have polled more than once");
    }

    /// The bounded re-mint wait RETURNS as soon as a freshly-minted JWT appears
    /// mid-poll — pins the common case: the refresher re-mints within the bound
    /// and the in-flight tool call proceeds normally.
    #[tokio::test]
    async fn await_remint_returns_jwt_once_it_appears() {
        let calls = std::cell::Cell::new(0u32);
        let out = await_remint_with(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n >= 3 {
                        Some("h.payload.s".to_string())
                    } else {
                        None
                    }
                }
            },
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(10),
        )
        .await;
        assert_eq!(out.as_deref(), Some("h.payload.s"));
        assert_eq!(calls.get(), 3, "returns on the first usable read, no more");
    }

    /// `AGENT_TOKENS` register / lookup / remove round-trip.
    #[test]
    fn agent_token_registry_round_trip() {
        let agent_id = uuid::Uuid::new_v4();
        assert!(lookup_agent_token(agent_id).is_none());
        let slot = std::sync::Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
            token: "tok".into(),
            jti: uuid::Uuid::nil(),
            exp: 0,
        }));
        register_agent_token(agent_id, slot);
        assert!(lookup_agent_token(agent_id).is_some());
        remove_agent_token(agent_id);
        assert!(
            lookup_agent_token(agent_id).is_none(),
            "lookup after remove must be None (fail-closed)"
        );
    }

    /// The agent-proxy `.mcp.json` shape matches the device proxy shape:
    /// loopback URL + nonce header, no baked Authorization bearer. Only the
    /// nonce's bound principal differs (Agent vs Device).
    #[test]
    fn write_coord_mcp_agent_proxy_config_emits_agent_bound_loopback_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-aproxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let wd = tmp.to_string_lossy().to_string();
        let agent_id = uuid::Uuid::new_v4();

        write_coord_mcp_agent_proxy_config(&wd, 31337, agent_id);

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:31337/coord-mcp");
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("agent proxy config must carry the nonce header");
        assert!(!nonce.is_empty());
        assert!(
            server["headers"].get("Authorization").is_none(),
            "agent proxy shape must NOT bake a static bearer: {written}"
        );
        // The nonce is bound to THIS agent.
        assert_eq!(
            proxy_principal_for_nonce(nonce),
            Some(ProxyPrincipal::Agent { agent_id })
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `coord_mcp_safe_to_write` — the non-clobber guard for the continuation
    /// provisioning path (continuations can degrade to a real repo checkout that
    /// already holds the operator's own `.mcp.json`).
    #[test]
    fn coord_mcp_safe_to_write_guards_user_config() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-safe-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        // 1. Absent → safe to create.
        assert!(coord_mcp_safe_to_write(&wd), "absent file must be writable");

        // 2. Solely our coord-mcp config → safe to refresh (we own it).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"https://c/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a solely-coord-mcp config is ours — refreshable"
        );

        // 3. A user's own config (different server) → must NOT clobber.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"my-server":{"type":"http","url":"https://x/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "a foreign mcpServers config must be left untouched"
        );

        // 4. coord-mcp ALONGSIDE another server (2 keys) → not solely ours → skip.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"url":"https://c/mcp"},"other":{"url":"x"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "coord-mcp plus a user server is the user's file — do not clobber"
        );

        // 5. Unparseable / non-JSON → conservatively do not clobber.
        std::fs::write(&mcp, "not json {{{").unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "an unparseable file must be left untouched"
        );

        // 6. Our coord-mcp config but bearer is an AGENT JWT → must NOT downgrade.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        std::fs::write(
            &mcp,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "must not downgrade an existing agent-JWT coord-mcp config to a device JWT"
        );

        // 7. Same shape but a DEVICE-JWT bearer → still refreshable (ours, same tier).
        let device_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
        let device_jwt = format!("h.{device_payload}.s");
        std::fs::write(
            &mcp,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {device_jwt}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a device-JWT coord-mcp config is ours at the same tier — refreshable"
        );

        // 8. The PROXY shape (loopback URL + nonce header, no Authorization) is
        //    ours at the device tier — refreshable. This is what every
        //    device-provisioned session's `.mcp.json` now looks like.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"abc123"}}}}"#,
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a proxy-shaped sole-coord-mcp config is ours — refreshable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orchestration of `provision_coord_mcp_with_jwt` — the exact write-decision
    /// body the gate-continuation path (`agent_runtime::run_continuation_terminal`)
    /// and every `acquire_for_terminal` chokepoint caller run once a bearer is
    /// resolved. Exercised through the JWT-injecting seam so it is deterministic
    /// regardless of whether the host has a real device JWT in its encrypted slot.
    /// Covers all four branches: device writes the device PROXY shape, agent
    /// writes the per-AGENT proxy shape (+ registers a live-token slot), a
    /// non-coord bearer is gated out, and a device bearer never downgrades an
    /// existing baked-agent-JWT config.
    #[test]
    fn provision_with_jwt_orchestration() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        // Build an unsigned JWT (`h.<payload>.s`) carrying just the `sub_type`
        // claim — all the device-arm orchestration inspects.
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        // An agent JWT additionally carries `sub` (= agent_id) + `exp` so the
        // converted agent arm can build the live-token slot.
        let mk_agent = |agent_id: uuid::Uuid| {
            let payload = URL_SAFE_NO_PAD.encode(
                format!(r#"{{"sub_type":"agent","sub":"{agent_id}","exp":9999999999}}"#).as_bytes(),
            );
            format!("h.{payload}.s")
        };
        let new_dir = || {
            let d = std::env::temp_dir().join(format!("coord-mcp-prov-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let mcp_of = |d: &std::path::Path| d.join(".mcp.json");
        let dev = mk("device");

        // A) device bearer + clean dir → provisions the loopback PROXY shape on
        //    the passed bound port: nonce header, NO static bearer.
        let d = new_dir();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, Some(19876));
        let written =
            std::fs::read_to_string(mcp_of(&d)).expect("device bearer must provision .mcp.json");
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(
            server["url"], "http://127.0.0.1:19876/coord-mcp",
            "device path must emit the loopback proxy URL on the passed port"
        );
        assert!(
            server["headers"]["X-Coord-Mcp-Proxy-Key"]
                .as_str()
                .is_some_and(|n| proxy_nonce_is_valid(n)),
            "device path must carry a registered per-session nonce"
        );
        assert!(
            server["headers"].get("Authorization").is_none(),
            "device path must NOT bake a static bearer (the proxy injects it live)"
        );
        let _ = std::fs::remove_dir_all(&d);

        // B) agent bearer + clean dir → provisions the per-AGENT PROXY shape
        //    (loopback URL + nonce, NO baked bearer) and registers the agent's
        //    live-token slot. The nonce is bound to THIS agent_id, and the proxy
        //    injects the agent's own refreshed token per request — never a static
        //    bearer, never the device token.
        let d = new_dir();
        let agent_id = uuid::Uuid::new_v4();
        let agent_jwt = mk_agent(agent_id);
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &agent_jwt, Some(19876));
        let written =
            std::fs::read_to_string(mcp_of(&d)).expect("agent bearer must provision .mcp.json");
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(
            server["url"], "http://127.0.0.1:19876/coord-mcp",
            "agent path now emits the per-agent loopback proxy URL"
        );
        assert!(
            server["headers"].get("Authorization").is_none(),
            "agent path must NOT bake a static bearer (the proxy injects it live)"
        );
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("agent path must carry a per-session nonce");
        assert_eq!(
            proxy_principal_for_nonce(nonce),
            Some(ProxyPrincipal::Agent { agent_id }),
            "the nonce must be bound to this agent_id"
        );
        assert!(
            lookup_agent_token(agent_id).is_some(),
            "the agent arm must register a live-token slot"
        );
        remove_agent_token(agent_id);
        let _ = std::fs::remove_dir_all(&d);

        // C) non-coord bearer (e.g. a Cognito access token, sub_type=access) →
        //    sub_type gate skips; no file is written (would 401 coord's verifier).
        let d = new_dir();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &mk("access"), Some(19876));
        assert!(
            !mcp_of(&d).exists(),
            "a non-device/agent bearer must NOT be written"
        );
        let _ = std::fs::remove_dir_all(&d);

        // D) device bearer into a dir already holding an AGENT-JWT coord-mcp config →
        //    the no-downgrade guard vetoes; the agent config is preserved verbatim.
        let d = new_dir();
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {}"}}}}}}}}"#,
            mk("agent")
        );
        std::fs::write(mcp_of(&d), &agent_cfg).unwrap();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, Some(19876));
        assert_eq!(
            std::fs::read_to_string(mcp_of(&d)).unwrap(),
            agent_cfg,
            "a device bearer must not downgrade an existing agent-JWT config"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3a — fail-closed: a DEVICE bearer with an UNKNOWN bound port
    /// (`None`) must NOT write a proxy `.mcp.json` (which would point at a dead
    /// bootstrap-default port), and must drop the degraded breadcrumb instead.
    /// This is the F1 root-cause guard.
    #[test]
    fn device_path_with_no_bound_port_writes_no_proxy_config() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-noport-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        // None bound port → fail-closed.
        provision_coord_mcp_with_jwt(&wd, &dev, None);

        assert!(
            !d.join(".mcp.json").exists(),
            "an unresolvable bound port must NOT write a (dead) proxy .mcp.json"
        );
        let crumb = std::fs::read_to_string(d.join(COORD_MCP_STATUS_FILE))
            .expect("a degraded breadcrumb must be written when the port is unresolvable");
        assert!(
            crumb.contains("coord-mcp UNREACHABLE") && crumb.contains("/gate"),
            "breadcrumb must be the actionable degraded line: {crumb}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3a — a non-degraded healthy DEVICE provision (a real `Some(port)`)
    /// must NOT leave a degraded breadcrumb from the write path itself (the
    /// async probe may add one later if the port is dead, but the synchronous
    /// write must not).
    #[test]
    fn device_path_with_bound_port_writes_proxy_and_no_synchronous_breadcrumb() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-port-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        provision_coord_mcp_with_jwt(&wd, &dev, Some(34567));

        assert!(
            d.join(".mcp.json").exists(),
            "a resolvable bound port must write the proxy .mcp.json"
        );
        let port = read_proxy_port(&wd);
        assert_eq!(port, Some(34567), "config must carry the passed bound port");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3b — persist→restore round-trip: a minted nonce is mirrored to the
    /// store, and `restore_proxy_nonces_from_store` re-loads it into a fresh
    /// in-memory map so it still validates after a (simulated) restart.
    #[test]
    fn persisted_nonce_survives_restore_round_trip() {
        // Inject a temp-dir store directly — NO process-global env mutation, so
        // this test cannot pollute sibling tests that read the default store.
        let (store_dir, store) = temp_store("nonce");

        // Mint a nonce in the live map, then mirror the snapshot to the INJECTED
        // store (the `register_proxy_nonce` body, split across its seams).
        let workdir = store_dir.join("session-wd").to_string_lossy().to_string();
        let (nonce, snapshot) =
            mint_and_register_nonce(&workdir, ProxyPrincipal::Device, NonceLifetime::Persistent);
        persist_proxy_nonces_with_store(&store, &snapshot);
        assert!(proxy_nonce_is_valid(&nonce));

        // It is actually on disk (independent of the in-memory map).
        let persisted = store.load_coord_mcp_nonces();
        assert_eq!(
            persisted.get(&nonce).map(String::as_str),
            Some(workdir.as_str()),
            "the minted nonce must be mirrored to the encrypted store"
        );

        // Simulate a restart: drop the nonce from the in-memory map, then
        // restore from the injected store via the same merge the boot path runs.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&nonce);
        }
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "precondition: the nonce is gone from the in-memory map"
        );
        restore_proxy_nonces_from(&store);
        assert!(
            proxy_nonce_is_valid(&nonce),
            "a persisted nonce must validate again after a restore"
        );

        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// OQ3 — an AGENT nonce is NEVER mirrored to the persisted store, while a
    /// DEVICE nonce in the same snapshot still is. A restarted runner thus has
    /// no live agent session AND no restored agent nonce, so an agent nonce
    /// hard-fails closed across a restart.
    #[test]
    fn agent_nonce_is_not_persisted_device_nonce_is() {
        let (store_dir, store) = temp_store("agent-nonce");

        // Mint an AGENT nonce, then persist the snapshot to the injected store.
        let agent_id = uuid::Uuid::new_v4();
        let agent_wd = store_dir.join("agent-wd").to_string_lossy().to_string();
        let (agent_nonce, snapshot) = mint_and_register_nonce(
            &agent_wd,
            ProxyPrincipal::Agent { agent_id },
            NonceLifetime::Persistent,
        );
        persist_proxy_nonces_with_store(&store, &snapshot);

        // Also mint a DEVICE nonce and persist.
        let dev_wd = store_dir.join("dev-wd").to_string_lossy().to_string();
        let (dev_nonce, snapshot) =
            mint_and_register_nonce(&dev_wd, ProxyPrincipal::Device, NonceLifetime::Persistent);
        persist_proxy_nonces_with_store(&store, &snapshot);

        let persisted = store.load_coord_mcp_nonces();
        assert!(
            !persisted.contains_key(&agent_nonce),
            "an agent nonce must NEVER be persisted to the encrypted store"
        );
        assert_eq!(
            persisted.get(&dev_nonce).map(String::as_str),
            Some(dev_wd.as_str()),
            "a device nonce in the same map must still be persisted"
        );

        // Cleanup the in-memory map entries.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&agent_nonce);
            map.remove(&dev_nonce);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 3b — `persist_proxy_nonces` honors the `nonce_persistence_enabled`
    /// gate: when persistence is disabled (test default, `COORD_MCP_PERSIST_NONCES`
    /// unset under `cfg(test)`), the DEFAULT-store path is a no-op. We assert the
    /// gate directly (a pure predicate) rather than mutating env + probing the
    /// real store — the injected-store seam makes the disk write deterministic and
    /// the gate is the only behavior worth pinning here.
    #[test]
    fn persistence_disabled_skips_default_store_write() {
        // Under cfg(test) with the env var unset, persistence is OFF by default
        // (see `nonce_persistence_enabled`), so `register_proxy_nonce`'s
        // default-store mirror is a guaranteed no-op — minting touches only the
        // in-memory map. A minted nonce is therefore valid in-memory but the
        // default store is never written. We verify the gate is the thing that
        // makes that true.
        assert!(
            !nonce_persistence_enabled(),
            "test builds must default persistence OFF so nonce-minting tests \
             never write the developer's real store"
        );
        // And minting still registers in-memory (the persist step is skipped).
        let (nonce, _snapshot) = mint_and_register_nonce(
            "/tmp/coord-mcp-persist-off-wd",
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
        );
        assert!(
            proxy_nonce_is_valid(&nonce),
            "minting must register the nonce in-memory regardless of persistence"
        );
    }

    /// Phase 3c — the pure reconcile predicate: rewrite only when a proxy port
    /// is present AND differs from the current bound port.
    #[test]
    fn reconcile_action_rewrites_only_on_port_mismatch() {
        assert_eq!(
            reconcile_action(Some(9877), 9876),
            ReconcileAction::Rewrite,
            "stale port → rewrite"
        );
        assert_eq!(
            reconcile_action(Some(9876), 9876),
            ReconcileAction::Leave,
            "matching port → leave"
        );
        assert_eq!(
            reconcile_action(None, 9876),
            ReconcileAction::Leave,
            "no readable proxy port (absent / static-bearer agent config) → leave"
        );
    }

    /// Phase 3c — end-to-end reconcile over a workdir set: a stale-port proxy
    /// config is rewritten to the bound port; a matching one and an agent
    /// (static-bearer) config are left untouched.
    #[test]
    fn reconcile_session_configs_rewrites_stale_leaves_agent() {
        let _env_lock = env_lock();
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let base = std::env::temp_dir().join(format!("coord-mcp-recon-{}", uuid::Uuid::now_v7()));

        // Point the root-config self-heal (Phase 5b) at `base` — which holds no
        // `.mcp.json` — so this test exercises ONLY the session path and never
        // touches the operator's real `qontinui-root/.mcp.json`. Restored below.
        std::fs::create_dir_all(&base).unwrap();
        let prev_root = std::env::var("QONTINUI_ROOT").ok();
        std::env::set_var("QONTINUI_ROOT", &base);

        // Stale proxy config on :9999 → must be rewritten to :9876.
        let stale = base.join("stale");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(
            stale.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9999/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"old"}}}}"#,
        )
        .unwrap();

        // Already-correct proxy config on :9876 → must be left as-is.
        let ok = base.join("ok");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::write(
            ok.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"keep"}}}}"#,
        )
        .unwrap();

        // Agent static-bearer config → must NEVER be clobbered.
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
        );
        let agent = base.join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join(".mcp.json"), &agent_cfg).unwrap();

        let workdirs = vec![
            stale.to_string_lossy().to_string(),
            ok.to_string_lossy().to_string(),
            agent.to_string_lossy().to_string(),
        ];
        let rewritten = reconcile_session_configs(workdirs, 9876);
        assert_eq!(
            rewritten, 1,
            "only the stale-port proxy config is rewritten"
        );

        // Stale rewritten to the bound port (+ a fresh registered nonce).
        assert_eq!(read_proxy_port(&stale.to_string_lossy()), Some(9876));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(stale.join(".mcp.json")).unwrap())
                .unwrap();
        let new_nonce = v["mcpServers"]["coord-mcp"]["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .unwrap();
        assert!(
            proxy_nonce_is_valid(new_nonce),
            "the rewritten config must carry a freshly-registered nonce"
        );

        // Correct config untouched; agent config preserved verbatim.
        assert_eq!(read_proxy_port(&ok.to_string_lossy()), Some(9876));
        assert_eq!(
            std::fs::read_to_string(agent.join(".mcp.json")).unwrap(),
            agent_cfg,
            "an agent static-bearer config must never be clobbered by the reconcile"
        );

        let _ = std::fs::remove_dir_all(&base);
        match prev_root {
            Some(p) => std::env::set_var("QONTINUI_ROOT", p),
            None => std::env::remove_var("QONTINUI_ROOT"),
        }
    }

    /// Phase 5b + plan 2026-07-07 Change 1 — the boot self-heal of the stale ROOT
    /// `.mcp.json` (the checked-in repo-root coord-mcp config). Drives the
    /// env-free `reconcile_root_config_at` so it neither mutates `QONTINUI_ROOT`
    /// nor touches the operator's real root config. Covers every dimension:
    /// wrong port (Rewrite), same-port-but-unregistered-nonce (ADOPT — Change 1
    /// core fix, file stays byte-identical), and the leave-alone cases (matching
    /// port + live nonce, absent file, foreign static-bearer).
    #[test]
    fn reconcile_root_config_self_heals_stale_root_mcp_json() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        // --- Case 1: stale PORT → Rewrite (client must reconnect anyway). ---
        let root = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9999/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"deadnonce"}}}}"#,
        )
        .unwrap();
        assert!(root_config_is_stale(&root, 9876), "wrong port is stale");
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::Rewrite,
            "a stale-port root config must be rewritten"
        );
        // Rewritten to the bound port with a freshly-registered nonce.
        assert_eq!(read_proxy_port(&root.to_string_lossy()), Some(9876));
        let new_nonce = read_proxy_nonce(&root.join(".mcp.json")).unwrap();
        assert!(
            proxy_nonce_is_valid(&new_nonce),
            "the rewritten root config must carry a freshly-registered nonce"
        );
        assert_ne!(new_nonce, "deadnonce");
        // And NO baked Authorization bearer (proxy shape).
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap())
                .unwrap();
        assert!(v["mcpServers"]["coord-mcp"]["headers"]
            .get("Authorization")
            .is_none());

        // --- Case 2: matching port + LIVE nonce → Leave (no rewrite). ---
        // The case-1 rewrite left a live nonce on port 9876; a second pass is a
        // no-op and must NOT mint a new nonce.
        assert!(
            !root_config_is_stale(&root, 9876),
            "matching port + live nonce is not stale"
        );
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::Leave,
            "a fresh root config must not be touched again"
        );
        assert_eq!(
            read_proxy_nonce(&root.join(".mcp.json")).unwrap(),
            new_nonce
        );

        // --- Case 3: matching port but UNREGISTERED nonce → ADOPT (no rewrite). ---
        // Plan 2026-07-07 Change 1 CORE FIX: re-register the EXACT on-disk nonce
        // rather than minting a fresh one, so a live MCP client's cached nonce
        // keeps validating. The `.mcp.json` MUST stay byte-identical.
        let dead = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dead).unwrap();
        let dead_body = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"notregistered-9c3f"}}}}"#;
        std::fs::write(dead.join(".mcp.json"), dead_body).unwrap();
        assert!(
            !proxy_nonce_is_valid("notregistered-9c3f"),
            "precondition: the on-disk nonce is not yet registered"
        );
        assert!(
            root_config_is_stale(&dead, 9876),
            "right port but an unregistered nonce needs healing"
        );
        assert_eq!(
            reconcile_root_config_at(&dead, 9876),
            RootReconcileAction::AdoptNonce,
            "an unregistered same-port nonce must be ADOPTED, not rewritten"
        );
        // The EXACT on-disk nonce is now valid (adopted verbatim into the registry).
        assert!(
            proxy_nonce_is_valid("notregistered-9c3f"),
            "the on-disk nonce must be adopted verbatim so a cached client validates"
        );
        // The file is byte-identical — no rewrite → live client cache preserved.
        assert_eq!(
            std::fs::read_to_string(dead.join(".mcp.json")).unwrap(),
            dead_body,
            "adopt must NOT rewrite the root .mcp.json"
        );
        // A second pass is now a no-op (the nonce is registered) → Leave.
        assert!(!root_config_is_stale(&dead, 9876));
        assert_eq!(
            reconcile_root_config_at(&dead, 9876),
            RootReconcileAction::Leave
        );

        // --- Case 4: absent root file → Leave. ---
        let empty = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            !root_config_is_stale(&empty, 9876),
            "absent file is not stale"
        );
        assert_eq!(
            reconcile_root_config_at(&empty, 9876),
            RootReconcileAction::Leave
        );
        assert!(!empty.join(".mcp.json").exists());

        // --- Case 5: foreign STATIC-BEARER (agent) root → never clobbered. ---
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
        );
        let agent = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join(".mcp.json"), &agent_cfg).unwrap();
        // A static-bearer shape has no proxy URL → not flagged stale (and even if
        // it were, `coord_mcp_safe_to_write` would refuse the rewrite).
        assert!(!root_config_is_stale(&agent, 9876));
        assert_eq!(
            reconcile_root_config_at(&agent, 9876),
            RootReconcileAction::Leave
        );
        assert_eq!(
            std::fs::read_to_string(agent.join(".mcp.json")).unwrap(),
            agent_cfg,
            "a static-bearer root config must never be clobbered"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dead);
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&agent);
    }

    /// THE REGRESSION, stated as a test — WRITER PATH 1 (boot self-heal).
    ///
    /// Plan 2026-07-20-ephemeral-runner-hijacks-root-mcp-json: an ephemeral
    /// runner boots on `:9877`, the unconditional root self-heal compares
    /// `9876 != 9877`, takes the `Rewrite` arm and claims the SHARED root
    /// `.mcp.json` for a process about to exit — leaving every root-opened
    /// Claude session on the machine with a dead coord-mcp endpoint (and hence
    /// no policy system) until the protected primary next boots, which can be
    /// days. Observed four times in the field (`:9881`, `:9877` ×3).
    ///
    /// Asserts on the FILE BYTES, not only the returned enum: the defect is a
    /// file write, so an action-only assertion would pass while the file was
    /// still clobbered.
    #[test]
    fn secondary_instance_never_self_heals_the_shared_root_config() {
        let root = std::env::temp_dir().join(format!("coord-mcp-sec-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        // A HEALTHY root config naming the primary's port — precisely what a
        // temp runner on a different port used to overwrite.
        let healthy = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"primary-owned-nonce"}}}}"#;
        std::fs::write(root.join(".mcp.json"), healthy).unwrap();

        // The SECONDARY on :9877 — the reproduction. Must not touch the file.
        assert_eq!(
            reconcile_root_config_gated(Some(&root), 9877, false),
            RootReconcileAction::SkippedSecondary,
            "a secondary must SKIP root self-heal, not evaluate it"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".mcp.json")).unwrap(),
            healthy,
            "the shared root .mcp.json must be BYTE-IDENTICAL after a secondary's boot reconcile"
        );

        // Belt-and-braces: the skip is not an artifact of the config being
        // healthy. A root config that IS stale for the secondary's port (the
        // `Rewrite` trigger) is still left alone.
        assert_eq!(
            root_reconcile_action(Some(9876), Some("primary-owned-nonce"), false, 9877),
            RootReconcileAction::Rewrite,
            "precondition: ungated, this input would have REWRITTEN the root config"
        );
        assert_eq!(
            reconcile_root_config_gated(Some(&root), 9877, false),
            RootReconcileAction::SkippedSecondary
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".mcp.json")).unwrap(),
            healthy,
            "even a would-be-Rewrite input must leave the shared root config untouched"
        );

        // The PRIMARY's shipped repair must be unchanged — the guard narrows who
        // may heal, never what healing does.
        assert_eq!(
            reconcile_root_config_gated(Some(&root), 9876, true),
            RootReconcileAction::AdoptNonce,
            "the primary still adopts an unregistered same-port nonce"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".mcp.json")).unwrap(),
            healthy,
            "adopt stays byte-identical for the primary too"
        );
        assert_eq!(
            reconcile_root_config_gated(Some(&root), 9881, true),
            RootReconcileAction::Rewrite,
            "the primary on a MOVED port still rewrites (shipped repair preserved)"
        );
        assert_eq!(read_proxy_port(&root.to_string_lossy()), Some(9881));

        // A secondary reports SkippedSecondary UNIFORMLY, including when no root
        // dir resolves. Reporting `Leave` there would lose the "a secondary
        // declined" signal exactly where an operator is already puzzled about why
        // nothing was repaired — so the instance check must precede the root-dir
        // match, not follow it.
        assert_eq!(
            reconcile_root_config_gated(None, 9877, false),
            RootReconcileAction::SkippedSecondary,
            "a secondary skips regardless of whether a root dir resolves"
        );
        // The primary with no resolvable root dir still reports Leave (unchanged).
        assert_eq!(
            reconcile_root_config_gated(None, 9876, true),
            RootReconcileAction::Leave,
            "no root dir to heal is Leave for the primary — shipped behaviour"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE REGRESSION, stated as a test — WRITER PATH 2 (the
    /// `coord_mcp_safe_to_write` chokepoint).
    ///
    /// `acquire_for_terminal` → `provision_coord_mcp_for_session` is a SECOND,
    /// independent writer: an operator tab opened at `D:/qontinui-root` on a temp
    /// runner rewrites the shared root config to the temp port without ever
    /// calling `reconcile_root_config`. Guarding only the boot self-heal would
    /// leave that hole open, so the guard also lands at the chokepoint every
    /// writer funnels through.
    ///
    /// Also pins the SCOPE of the refusal: a secondary keeps full authority over
    /// every OTHER workdir. That is not incidental — the per-repo sibling configs
    /// surviving a root hijack are what make in-session recovery possible (probe
    /// the siblings for one that still answers, copy it over root).
    #[test]
    fn shared_root_write_guard_refuses_only_the_root_dir_and_only_for_secondaries() {
        let root = Path::new("D:/qontinui-root");
        let sibling = "D:/qontinui-root/qontinui-runner";
        let worktree = "D:/qontinui-root/wt-something";

        // PRIMARY (owns shared root state) — unchanged everywhere, root included.
        for wd in ["D:/qontinui-root", sibling, worktree] {
            assert!(
                shared_root_write_allowed_at(wd, Some(root), true),
                "the primary must keep writing {wd}"
            );
        }

        // SECONDARY — refused at root ONLY.
        assert!(
            !shared_root_write_allowed_at("D:/qontinui-root", Some(root), false),
            "a secondary must never claim the shared root config"
        );
        assert!(
            shared_root_write_allowed_at(sibling, Some(root), false),
            "a secondary still provisions per-repo sibling configs (the recovery asset)"
        );
        assert!(
            shared_root_write_allowed_at(worktree, Some(root), false),
            "a secondary still provisions its own isolated worktree sessions"
        );

        // Path-shape robustness — the refusal must not be defeatable by a
        // trailing separator, backslashes, or (on Windows) a case difference.
        for variant in [
            "D:/qontinui-root/",
            "D:\\qontinui-root",
            "D:\\qontinui-root\\",
        ] {
            assert!(
                !shared_root_write_allowed_at(variant, Some(root), false),
                "{variant} is the shared root and must be refused"
            );
        }
        #[cfg(windows)]
        assert!(
            !shared_root_write_allowed_at("d:/QONTINUI-ROOT", Some(root), false),
            "Windows paths are case-insensitive — the guard must be too"
        );

        // A path that merely SHARES A PREFIX with the root is not the root.
        assert!(
            shared_root_write_allowed_at("D:/qontinui-root-other", Some(root), false),
            "prefix-sharing must not be mistaken for path identity"
        );

        // No resolvable umbrella root → nothing to protect, nothing refused.
        assert!(shared_root_write_allowed_at(
            "D:/qontinui-root",
            None,
            false
        ));
    }

    /// The chokepoint guard composes with the pre-existing non-clobber guard
    /// rather than replacing it: `coord_mcp_safe_to_write` still answers `true`
    /// for an ordinary workdir in the (primary) test process, so wiring the new
    /// refusal in front of it did not change the shipped behaviour for the case
    /// that matters — a secondary provisioning its own session cwd.
    #[test]
    fn safe_to_write_still_permits_an_ordinary_workdir() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-ok-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "an absent .mcp.json in a non-root workdir stays writable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Plan 2026-07-07 Change 1 — the pure `root_reconcile_action` resolver over
    /// explicit inputs, isolating the adopt-vs-rewrite-vs-leave decision from any
    /// file I/O or the process-global nonce map.
    #[test]
    fn root_reconcile_action_resolves_adopt_vs_rewrite_vs_leave() {
        // Not our shape (no readable proxy port) → Leave regardless of nonce.
        assert_eq!(
            root_reconcile_action(None, Some("x"), false, 9876),
            RootReconcileAction::Leave
        );
        // Port moved → Rewrite (client's cached URL is stale too — reconnect).
        assert_eq!(
            root_reconcile_action(Some(9999), Some("x"), true, 9876),
            RootReconcileAction::Rewrite,
            "a moved port must rewrite even with a registered nonce"
        );
        // Same port, nonce readable but UNregistered → Adopt (the core fix).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), false, 9876),
            RootReconcileAction::AdoptNonce
        );
        // Same port, nonce readable AND registered → Leave (healthy).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), true, 9876),
            RootReconcileAction::Leave
        );
        // Same port, NO nonce readable → Rewrite (nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), None, false, 9876),
            RootReconcileAction::Rewrite
        );
        // Same port, EMPTY nonce string → Rewrite (empty is nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), Some(""), false, 9876),
            RootReconcileAction::Rewrite
        );
    }

    /// Plan 2026-07-07 Change 1 — `adopt_on_disk_nonce` re-registers the EXACT
    /// string as a live Device binding so a subsequent proxy request with that
    /// string is accepted (the gate's nonce check passes), WITHOUT minting a new
    /// nonce. Mirrors the restart-survival contract at the unit level.
    #[test]
    fn adopt_on_disk_nonce_reregisters_exact_string_as_device() {
        let workdir = format!("D:/adopt-wt-{}", uuid::Uuid::now_v7());
        let nonce = format!("ondisk-{}", uuid::Uuid::new_v4().simple());
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "precondition: not registered"
        );

        adopt_on_disk_nonce(&workdir, &nonce);

        assert!(
            proxy_nonce_is_valid(&nonce),
            "the exact on-disk nonce must validate after adoption"
        );
        assert_eq!(
            proxy_principal_for_nonce(&nonce),
            Some(ProxyPrincipal::Device),
            "an adopted nonce is bound to the Device principal"
        );
    }

    /// Plan 2026-07-07 Change 3 — a DEVICE nonce evicted by a same-workdir
    /// re-mint stays valid through its grace TTL (so an in-flight client rides
    /// through), while the fresh nonce is live. An AGENT nonce is NEVER graced.
    #[test]
    fn remint_graces_evicted_device_nonce_but_never_agent() {
        // Device: mint A, then re-mint B for the SAME workdir → A graced, B live.
        let wd = format!("D:/grace-wt-{}", uuid::Uuid::now_v7());
        let a = register_proxy_nonce(&wd);
        assert!(proxy_nonce_is_valid(&a));
        let b = register_proxy_nonce(&wd);
        assert_ne!(a, b);
        assert!(proxy_nonce_is_valid(&b), "the fresh device nonce is live");
        assert!(
            proxy_nonce_is_valid(&a),
            "the evicted device nonce rides the grace TTL"
        );
        assert_eq!(
            proxy_principal_for_nonce(&a),
            Some(ProxyPrincipal::Device),
            "a graced nonce resolves to Device (no scope elevation)"
        );

        // Agent: mint A, re-mint B for the SAME workdir → A NOT graced (fails closed).
        let awd = format!("D:/grace-agent-wt-{}", uuid::Uuid::now_v7());
        let agent_id = uuid::Uuid::new_v4();
        let a2 = register_agent_proxy_nonce(&awd, agent_id);
        assert!(proxy_nonce_is_valid(&a2));
        let b2 = register_agent_proxy_nonce(&awd, agent_id);
        assert_ne!(a2, b2);
        assert!(proxy_nonce_is_valid(&b2), "the fresh agent nonce is live");
        assert!(
            !proxy_nonce_is_valid(&a2),
            "an evicted AGENT nonce must hard-fail closed — never graced"
        );
    }

    /// Plan 2026-07-07 Change 3 — grace fails closed exactly at the deadline:
    /// an entry whose `expires_at` is not strictly in the future is treated as
    /// expired and lazily evicted. Deterministic because monotonic time only
    /// advances — an entry stamped `now()` is never `> now()` at the later check.
    #[test]
    fn graced_nonce_expires_and_is_lazily_evicted() {
        let nonce = format!("expired-{}", uuid::Uuid::new_v4().simple());
        graced_nonces().lock().unwrap().insert(
            nonce.clone(),
            GracedNonce {
                expires_at: std::time::Instant::now(),
            },
        );
        assert!(
            !graced_nonce_is_valid(&nonce),
            "an already-elapsed grace entry must be invalid"
        );
        assert!(
            !graced_nonces().lock().unwrap().contains_key(&nonce),
            "an expired grace entry must be lazily evicted on the failing check"
        );
    }

    /// Phase 4 (plan 2026-07-27-coord-mcp-flake-remediation, R6): every
    /// rotation event appends exactly one JSONL forensics line, and no line
    /// ever carries a full nonce — only the 8-char prefix.
    #[test]
    fn rotation_forensics_one_line_per_event_and_prefix_only() {
        // Shared across every file-asserting forensics test (see
        // `rotation_log_test_dir`) — peer tests append lines for OTHER
        // workdirs, so every assertion below filters by this test's own.
        let dir = rotation_log_test_dir();

        let wd = format!("D:/rot-forensics-wt-{}", uuid::Uuid::now_v7());
        let a = register_proxy_nonce(&wd); // mint
        let b = register_proxy_nonce(&wd); // mint + evict(a) + grace(a)
        evict_proxy_nonces_for_workdir(&wd); // evict(b) + grace(b)
        let adopted = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        adopt_on_disk_nonce(&wd, &adopted); // adopt (nothing left to evict)

        // A real `.mcp.json` write into a temp workdir → one "write" line.
        let wt = std::env::temp_dir().join(format!("coord-mcp-rot-wt-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&wt).unwrap();
        let wt_str = wt.to_string_lossy().to_string();
        write_mcp_json(&wt_str, &coord_mcp_proxy_config_json(9876, &adopted));

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let mine: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
            .filter(|v| v["workdir"] == wd.as_str() || v["workdir"] == wt_str.as_str())
            .collect();

        let count = |event: &str| mine.iter().filter(|v| v["event"] == event).count();
        assert_eq!(count("mint"), 2, "one line per mint");
        assert_eq!(count("evict"), 2, "re-mint evicted `a`, close evicted `b`");
        assert_eq!(count("grace"), 2, "each evicted device nonce graced once");
        assert_eq!(count("adopt"), 1, "one line per adoption");
        assert_eq!(count("write"), 1, "one line per .mcp.json write");
        assert_eq!(mine.len(), 8, "no extra lines for these workdirs");

        // Prefix-only guarantee: the key field is exactly the 8-char prefix,
        // and no full 64-char-class nonce appears anywhere in the file.
        let key_re = regex::Regex::new("^[0-9a-f]{8}$").unwrap();
        for v in &mine {
            let key = v["key_prefix"].as_str().expect("key_prefix is a string");
            assert!(
                key_re.is_match(key),
                "key field must be the 8-char prefix only, got {key:?}"
            );
        }
        for full in [a.as_str(), b.as_str(), adopted.as_str()] {
            assert!(!raw.contains(full), "a forensics line leaked a full nonce");
        }
        // Length-class guard scoped to THIS test's lines (`mine`): a
        // concurrent test whose workdir string embeds 64 hex chars must not
        // nondeterministically fail this test. The `raw.contains` checks
        // above remain the whole-file leak tripwire for real nonces.
        let hex64 = regex::Regex::new("[0-9a-f]{64}").unwrap();
        for v in &mine {
            assert!(
                !hex64.is_match(&v.to_string()),
                "no line may contain a full 64-char-class nonce"
            );
        }
    }

    /// Follow-up to Phase 4: a REJECTED proxy request is the consumer half of
    /// the trail. It must emit a `reject` line carrying the prefix (so it joins
    /// to the `evict` line that killed the key), name the workdir when the
    /// nonce is still bound, and leak no more key material than any other line.
    #[test]
    fn rotation_forensics_reject_line_joins_to_the_evicting_workdir() {
        let dir = rotation_log_test_dir();

        let wd = format!("D:/rot-reject-wt-{}", uuid::Uuid::now_v7());
        let live = register_proxy_nonce(&wd);
        // A key this runner never minted — the shape a client presents after
        // its own was evicted and the registry moved on.
        let stranger = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );

        log_proxy_nonce_rejected(Some(&live), "bound but gated (401)");
        log_proxy_nonce_rejected(Some(&stranger), "unregistered (401)");

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let rejects: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
            .filter(|v| v["event"] == "reject")
            .filter(|v| {
                v["key_prefix"] == rotation_key_prefix(&live).as_str()
                    || v["key_prefix"] == rotation_key_prefix(&stranger).as_str()
            })
            .collect();
        assert_eq!(rejects.len(), 2, "one line per distinct rejected key");

        let for_key = |n: &str| {
            rejects
                .iter()
                .find(|v| v["key_prefix"] == rotation_key_prefix(n).as_str())
                .expect("a reject line for this key")
                .clone()
        };
        // A still-registered nonce names its workdir directly...
        assert_eq!(for_key(&live)["workdir"], wd.as_str());
        // ...an unknown one cannot, and is joined on `key_prefix` instead.
        assert_eq!(for_key(&stranger)["workdir"], "");

        for n in [live.as_str(), stranger.as_str()] {
            assert!(
                !raw.contains(n),
                "a reject line leaked a full nonce — prefixes only"
            );
        }
    }

    /// The reject path runs on the REQUEST path, so a client looping against a
    /// dead key must not append a line per attempt: one line per key per
    /// window, with the suppressed repeats counted into the next one.
    #[test]
    fn reject_throttle_admits_once_per_window_and_counts_suppressed() {
        let prefix = format!("thr{}", &uuid::Uuid::new_v4().simple().to_string()[..5]);

        assert_eq!(
            reject_throttle_admit(&prefix),
            Some(0),
            "the first reject for a prefix always emits, with nothing suppressed yet"
        );
        for _ in 0..5 {
            assert_eq!(
                reject_throttle_admit(&prefix),
                None,
                "repeats inside the window stay silent"
            );
        }

        // Reopen the window by backdating the entry rather than sleeping for
        // REJECT_LOG_THROTTLE — the throttle is a duration comparison, so this
        // exercises the same branch without a minute of wall-clock.
        {
            let mut map = REJECT_THROTTLES
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap();
            let t = map.get_mut(&prefix).expect("entry for this prefix");
            t.last_logged =
                std::time::Instant::now() - REJECT_LOG_THROTTLE - std::time::Duration::from_secs(1);
        }
        assert_eq!(
            reject_throttle_admit(&prefix),
            Some(5),
            "the next emission after the window carries the suppressed count"
        );
        assert_eq!(
            reject_throttle_admit(&prefix),
            None,
            "and it opened a fresh window"
        );
    }
}
