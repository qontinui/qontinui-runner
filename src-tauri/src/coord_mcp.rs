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
//!   and fall back to the legacy default slot, the pre-B3 behavior — EXCEPT
//!   where the machine cannot state its tenant by any route, which is refused
//!   outright (see `session_tenant_or_refuse`).

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
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
///   revoked the moment the machine is opted back out
///   ([`session_identity_marker_present`]).
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
    /// The tenant pin observed when this binding was created
    /// (`machine.json::active_tenant_id` — the same value
    /// `stamp_session_tenant` records on the session's coord row). Unused for
    /// Agent nonces — their bearer is the agent JWT, whose tenant claim is
    /// frozen at mint.
    ///
    /// **PROVENANCE TELEMETRY since plan
    /// `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
    /// Phase 1b.** It used to be the sole authority over which credential slot
    /// the DEVICE proxy forwarded, and that was the defect: the value is frozen
    /// at creation, but only ONE of the three creation paths
    /// ([`mint_and_register_nonce`]) actually samples the machine. The other
    /// two ([`restore_proxy_nonces_from`], [`adopt_on_disk_nonce`]) hardcoded
    /// `Unpinned`, which selected the legacy `access_token` slot — so a session
    /// provisioned against a per-tenant slot silently swapped credentials at
    /// every runner restart, with nothing observable to say so.
    ///
    /// [`session_tenant_or_refuse`] now resolves the tenant at REQUEST time.
    /// This field keeps exactly one authority, and only because nothing else
    /// can supply it: when it reads `Pinned(t)` it NAMES the session's own
    /// tenant, which is the only way to tell two co-resident tenants apart on a
    /// multi-tenant device (`machine.json` records a single *active* tenant).
    /// `Unpinned` and `Unresolvable` no longer select anything — see that
    /// function's authority table.
    ///
    /// **Typed since the memory-injection plan's Phase 3.** This was an
    /// `Option<Uuid>`, whose `None` conflated three very different things: a
    /// legitimate single-tenant install, a nonce RESTORED from the store, a
    /// nonce ADOPTED off disk — and, once the machine's own pin could fail,
    /// a machine that cannot state its tenant at all. Only the last of those
    /// may fail closed, so the distinction has to survive in the binding
    /// rather than being reconstructed later (it cannot be).
    session_pin: crate::session::tenant_pin::TenantPin,
    /// The runner TERMINAL this nonce was provisioned for, frozen at mint time
    /// (same pattern and lifetime as `session_tenant`).
    ///
    /// **Why the terminal and not the workdir.** Caller self-identification
    /// (session-fabric Phase 0) resolves `nonce → … → coord agent_session_id`.
    /// The `workdir` leg cannot be deterministic: a workdir is NOT unique — on
    /// the operator's box 8 live sessions share one repo dir — so
    /// workdir → session is 1:N and any pick is a guess. The TERMINAL is the
    /// runner's only 1:1 handle: the runner spawns the PTY, mints this nonce for
    /// it, and the durable lifecycle record already carries `terminal_id` beside
    /// `claude_session_id` (`session::session_lifecycle_store`). So a binding
    /// that knows its terminal resolves EXACTLY, with no tie-break.
    ///
    /// `None` for the bindings that genuinely have no terminal: the in-cwd
    /// `.mcp.json` writer (that file is shared by every session in the cwd by
    /// construction), the `/coord-mcp/provision-session` mint route (bare
    /// sessions the runner did not spawn), and the on-disk adopt (that form
    /// carries no terminal). Those keep the workdir leg as their fallback.
    ///
    /// The boot restore is NO LONGER in that list: the persisted store was
    /// widened to carry the terminal, so a restored binding reproduces it.
    /// That is an EVICTION-KEY fidelity fix, not an identity claim — the PTY
    /// it names died with the previous runner process, so self-identification
    /// still misses on a restored nonce. Read via [`terminal_id_for_nonce`].
    terminal_id: Option<String>,
    /// Wall-clock time this binding was MINTED — its true age, carried across
    /// restarts by [`crate::secure_storage::StoredNonceBinding::minted_at_unix`]
    /// rather than reset at every boot. **Not** a credential property: nothing
    /// checks it to decide validity (a Persistent nonce has no expiry,
    /// deliberately).
    ///
    /// Its one job is to give [`device_nonce_snapshot`]'s bound a total,
    /// oldest-first eviction order. Before that bound existed, the persisted
    /// device set had exactly one production reaper —
    /// [`release_workdir_on_session_close`], on the last open record for a
    /// workdir closing — **and it returns early on the `qontinui-root`
    /// workdir**. Nothing else reached it: `revoke_proxy_nonce` has no
    /// production caller, `evict_proxy_nonces_for_workdir` fires only on
    /// relay-chat close for a per-session relay workdir, terminal close revokes
    /// nothing, and — since Phase 4 carried `terminal_id` into the store — a
    /// persistent binding is otherwise evicted only by a re-mint for its own
    /// `(workdir, terminal_id)` slot. Terminal ids are per-PTY, so a slot is
    /// never reused and every terminal spawned in ROOT added one permanent
    /// entry, restored into the live map at every boot, with no expiry column
    /// and no cap: monotone growth in an encrypted store that is fully
    /// re-encrypted and rewritten on every mint, and an ever-growing set of
    /// eternally-valid loopback keys.
    ///
    /// A restored binding carries its PERSISTED mint time. It has to: stamping
    /// the restore instant instead made every restored binding tie, so the
    /// moment the restored pool alone exceeded the cap the "oldest-first" cut
    /// fell entirely to the `nonce`-string tiebreak — a uniformly random pick
    /// over hex strings, across a pool that mixes long-dead terminals with
    /// sessions alive right now that survived the restart. That is precisely
    /// the live-session orphaning the cap exists to prevent, and it engaged
    /// exactly when the cap did.
    ///
    /// A binding whose store entry predates the timestamp field (a pre-Phase-4
    /// bare string, or a modern entry written before the widening) restores as
    /// [`std::time::SystemTime::UNIX_EPOCH`]: its true age is unrecoverable,
    /// but it is certainly older than anything a timestamp-writing binary
    /// minted, so ordering it OLDEST is the honest reading — and, unlike a
    /// `now()` fallback, it does not promote unknown-age cruft above live
    /// sessions. Such entries tie among themselves and still fall to the nonce
    /// tiebreak; that residue is bounded to the one-time upgrade window,
    /// because every subsequent snapshot persists a real time for every
    /// binding minted since.
    minted_at: std::time::SystemTime,
}

// ---------------------------------------------------------------------------
// Session-provisioned coord identity: the TWO gates
// (plan 2026-07-17 §1/§3, re-cut by plan
//  2026-08-24-headless-box-has-no-working-coord-credential-door Phase 1)
// ---------------------------------------------------------------------------
//
// The two gates are now the SAME-USER HANDSHAKE and the OPT-IN MARKER. The
// original §1 shape carried a spawn-time master env flag
// (`QONTINUI_SESSION_COORD_IDENTITY_ENABLED`) instead of the handshake; it was
// DELETED, not deprecated, and no override was left behind. Two reasons, both
// load-bearing:
//
//  1. It was read from the process environment, so it was fixed for a runner's
//     entire lifetime — an operator could neither enable NOR revoke it without
//     restarting the runner, which served policy `production-and-cost`
//     `runner-lifecycle` forbids. It could therefore never be the operator's
//     off switch; the marker already is one, live and per request.
//  2. It gated the wrong thing. A flag says "the feature is on"; it says
//     nothing about WHO is calling. Flag-on left the route gated on marker
//     *presence* alone, and presence is not identity — see below.
//
// What replaced it is a control the flag never provided: proof that the caller
// is the SAME OS USER the runner runs as.

/// File name of the per-machine operator opt-in marker, under `~/.qontinui/`.
/// Its mere existence is the signal; contents are never read.
///
/// Re-exported from the LIB crate ([`qontinui_runner_lib::profile_cli`]) so this
/// authoritative runner-side gate and the standalone `qontinui-shim` `.exe` share
/// ONE source of truth for the marker — a rename can no longer silently desync
/// the two processes.
pub(crate) use qontinui_runner_lib::profile_cli::SESSION_IDENTITY_MARKER_FILE;

/// The loopback handshake contract, re-exported from the LIB crate for exactly
/// the same reason as the marker above: the runner BIN WRITES the key and the
/// standalone `qontinui-shim` `.exe` READS it, and the two must agree on the
/// path and the header name byte-for-byte. See
/// [`qontinui_runner_lib::profile_cli::RUNNER_LOOPBACK_KEY_FILE`] for why the
/// handshake exists at all.
pub(crate) use qontinui_runner_lib::profile_cli::{
    runner_loopback_key_path, RUNNER_LOOPBACK_KEY_FILE, RUNNER_LOOPBACK_KEY_HEADER,
};

/// Absolute path of the opt-in marker (`~/.qontinui/allow-session-coord-identity`).
/// `None` when the home dir is unresolvable — which [`session_identity_gate`]
/// treats as NOT opted in (fail-closed: an unresolvable home must never read as
/// consent). Delegates to the shared lib resolver so the gate and the shim
/// compute the identical path (directory + filename), not merely the filename.
pub(crate) fn session_identity_marker_path() -> Option<std::path::PathBuf> {
    qontinui_runner_lib::profile_cli::session_identity_marker_path()
}

/// THIS runner start's loopback handshake secret, held in memory.
///
/// The file at [`runner_loopback_key_path`] is the DELIVERY channel; this cell
/// is the truth the gate compares against. Seeded exactly once per process by
/// [`init_loopback_handshake_key`] at server start, so:
///
/// * a caller that cannot read the owner-only file cannot learn the secret, and
/// * a failed file write leaves the route denying every request (fail-closed)
///   rather than accepting an empty or attacker-chosen value.
///
/// Empty/unset ⇒ the route is closed. That is the honest posture for a bin or a
/// unit test that never initialized one.
static LOOPBACK_HANDSHAKE_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Generate this start's handshake secret and publish it owner-only at
/// [`runner_loopback_key_path`]. Idempotent per process (the `OnceLock` is the
/// guard), and best-effort on the write: a write failure is warned about and
/// leaves the route CLOSED, never open.
///
/// Called synchronously from [`crate::mcp_api::start_server`] before the socket
/// is served, so a caller that can reach `/health` can already read the key.
///
/// # Entropy and rotation
///
/// 32 bytes from the OS CSPRNG, hex-encoded (64 chars) — well past the ">=32
/// bytes of entropy" the contract requires, and the same shape the doors
/// already handle for nonces. Rotated per runner start (see
/// [`RUNNER_LOOPBACK_KEY_FILE`]), which is what bounds a leak's blast radius.
///
/// The secret itself is NEVER logged — the log line names the path and a short
/// prefix only, matching the rotation log's discipline.
pub(crate) fn init_loopback_handshake_key() {
    let secret = LOOPBACK_HANDSHAKE_KEY.get_or_init(|| {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        // `rand::rng()` is the thread CSPRNG (ChaCha12, seeded from the OS
        // entropy source and reseeded periodically) — a cryptographically
        // secure generator, not the `SmallRng`/`StdRng` distinction. It is the
        // infallible one: `OsRng` in rand 0.9 is `TryRngCore`, and there is no
        // sane weaker fallback for a secret, so taking the infallible CSPRNG is
        // both simpler and strictly correct here.
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    });
    let Some(path) = runner_loopback_key_path() else {
        warn!(
            "coord_mcp: home dir unresolvable — no loopback handshake key written; \
             POST /coord-mcp/provision-session will deny every request"
        );
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "coord_mcp: could not create {} for the loopback handshake key: {e} — \
                 POST /coord-mcp/provision-session will deny every request",
                parent.display()
            );
            return;
        }
        // Harden the DIRECTORY too, not just the file. `write_owner_only`
        // opens with `create(true).truncate(true)` and no `O_NOFOLLOW`, so it
        // FOLLOWS a symlink at `path`. A group- or world-writable `~/.qontinui`
        // (a `create_dir_all` under the default umask yields 0775/0755, which is
        // exactly what this box had) therefore lets a non-owner pre-plant
        // `runner-loopback-key` as a symlink and read the secret we are about to
        // write through it — defeating the whole same-user proof. 0700 removes
        // the planting vector; it also stops a local user LISTING the dir to
        // learn whether the opt-in marker exists, which the HTTP gate
        // deliberately withholds from unauthenticated callers.
        //
        // Best-effort and non-fatal for the same reason the write is: a failure
        // here leaves the route denying every request, never opening it.
        if let Err(e) = crate::fs_perms::restrict_dir_to_owner(parent) {
            warn!(
                "coord_mcp: could not restrict {} to owner-only: {e} — the loopback \
                 handshake key's parent stays group/world-accessible",
                parent.display()
            );
        }
    }
    match crate::fs_perms::write_owner_only(&path, secret.as_bytes()) {
        Ok(()) => info!(
            "coord_mcp: loopback handshake key rotated for this runner start \
             (path={}, key_prefix={}…)",
            path.display(),
            &secret[..8.min(secret.len())]
        ),
        Err(e) => warn!(
            "coord_mcp: failed to write the loopback handshake key to {}: {e} — \
             POST /coord-mcp/provision-session will deny every request (fail-closed)",
            path.display()
        ),
    }
}

/// This process's handshake secret, or `None` when none was ever initialized.
fn loopback_handshake_key() -> Option<&'static str> {
    LOOPBACK_HANDSHAKE_KEY
        .get()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
}

/// Constant-time byte equality. Deliberately NOT `==`: a short-circuiting
/// comparison on a secret leaks its prefix through timing, and this route is
/// reachable by any local process, so the oracle is genuinely available.
/// Backed by `subtle`, already a dependency.
fn secret_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    // `ct_eq` requires equal lengths; the length itself is not a secret (the
    // key is a fixed-width hex string), so comparing it first is safe and is
    // what every constant-time string compare does.
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Why the mint route refused. Typed rather than a bare bool so the route can
/// return an explicit, actionable reason — the runner's "no silent empty
/// responses" rule. A denied caller must be able to tell "you did not prove you
/// are the same user" from "you proved it with the WRONG secret" from "this
/// machine has not opted in", because all three have different fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionIdentityDenial {
    /// No `X-Qontinui-Loopback-Key` header (or an empty one) — the caller never
    /// attempted the same-user handshake. Fix: read the key file.
    NoHandshake,
    /// A handshake was presented and it is not this runner start's secret.
    /// Fix: re-read the key file (it rotates per runner start) — or the caller
    /// is a different local user, and the denial is working as designed.
    HandshakeMismatch,
    /// Same-user proven, but the operator has not dropped the opt-in marker.
    NotOptedIn,
}

impl SessionIdentityDenial {
    /// Machine-readable code for the route's JSON error body.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            SessionIdentityDenial::NoHandshake => "COORD_MCP_PROVISION_NO_HANDSHAKE",
            SessionIdentityDenial::HandshakeMismatch => "COORD_MCP_PROVISION_HANDSHAKE_MISMATCH",
            SessionIdentityDenial::NotOptedIn => "COORD_MCP_PROVISION_NOT_OPTED_IN",
        }
    }

    /// Human/agent-actionable explanation — names the exact lever to flip.
    pub(crate) fn message(&self) -> String {
        let key_path = || {
            runner_loopback_key_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| format!("~/.qontinui/{RUNNER_LOOPBACK_KEY_FILE}"))
        };
        match self {
            SessionIdentityDenial::NoHandshake => format!(
                "no same-user handshake presented — send the contents of {} in the \
                 {RUNNER_LOOPBACK_KEY_HEADER} header (the file is owner-only, so only \
                 the user this runner runs as can read it)",
                key_path()
            ),
            SessionIdentityDenial::HandshakeMismatch => format!(
                "the {RUNNER_LOOPBACK_KEY_HEADER} handshake does not match this runner \
                 start's key — re-read {} (it is rotated on every runner start, so a \
                 cached value goes stale when the runner restarts)",
                key_path()
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
/// fail-closed posture is unit-testable without touching process-global state,
/// the real home dir, or a live `OnceLock`.
///
/// Order is deliberate: the **handshake first**, the marker second. The
/// handshake is the identity proof, and a caller who has not proven they are
/// the owning user should not learn from the response whether this machine
/// happens to be opted in.
fn resolve_session_identity_gate(
    presented_key: Option<&str>,
    expected_key: Option<&str>,
    marker_exists: bool,
) -> Result<(), SessionIdentityDenial> {
    let presented = presented_key.map(str::trim).filter(|s| !s.is_empty());
    let Some(presented) = presented else {
        return Err(SessionIdentityDenial::NoHandshake);
    };
    // No key initialized on this runner (home unresolvable, or the write
    // failed) ⇒ nothing can match. Fail CLOSED rather than accepting anything.
    let Some(expected) = expected_key.filter(|s| !s.is_empty()) else {
        return Err(SessionIdentityDenial::HandshakeMismatch);
    };
    if !secret_eq(presented.as_bytes(), expected.as_bytes()) {
        return Err(SessionIdentityDenial::HandshakeMismatch);
    }
    if !marker_exists {
        return Err(SessionIdentityDenial::NotOptedIn);
    }
    Ok(())
}

/// The authorization gate for session-provisioned coord identity: the SAME-USER
/// handshake AND the per-machine opt-in marker. BOTH are required — neither
/// alone grants identity.
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
/// `127.0.0.1:9876` is a TCP socket, and any local user can connect to loopback
/// — there is no peer-credential check on a TCP socket. So reaching the port
/// proves nothing, and marker *presence* does not fix that: it is a property of
/// the machine, not of the caller. Without the handshake, a DIFFERENT local user
/// could mint a device-scoped nonce they provably cannot obtain today, because
/// the store (`auth_tokens.enc`) is owner-only and closed to them. The handshake
/// file is written owner-only, so requiring its contents reproduces the FILE's
/// boundary on the SOCKET — that is the whole mechanism.
///
/// The marker then answers the second, different question: has the operator
/// deliberately, revocably opted this machine in? A same-user compromise (a
/// dependency's post-install script runs as the operator and can read the key
/// file) is bounded by the marker, not by the handshake.
///
/// # Live, not just mint-time
///
/// The MARKER half is re-checked on every request that presents an
/// [`NonceLifetime::Ephemeral`] nonce ([`live_binding`] →
/// [`session_identity_marker_present`]), so deleting the marker REVOKES
/// already-minted session nonces instead of merely blocking new ones. It is the
/// operator's actual off switch. Cheap by construction: only ephemeral bindings
/// pay the check, so a runner-spawned terminal never does.
pub(crate) fn session_identity_gate(
    presented_key: Option<&str>,
) -> Result<(), SessionIdentityDenial> {
    // One marker stat per mint attempt. The mint route runs at most once per
    // session launch, so the stat is free — and the resolver's ordering
    // (handshake first) still guarantees an UNAUTHENTICATED caller never learns
    // from the denial whether this machine is opted in.
    resolve_session_identity_gate(
        presented_key,
        loopback_handshake_key(),
        session_identity_marker_present(),
    )
}

/// Does the per-machine opt-in marker exist RIGHT NOW?
///
/// This is the live-revocation half of [`session_identity_gate`], split out
/// because [`live_binding`] has no request headers to hand: it re-checks the
/// operator's off switch on every request that presents an ephemeral nonce, and
/// the handshake was already proven at mint time by the route. Deleting the
/// marker therefore invalidates ALREADY-MINTED session nonces, not merely future
/// mints.
///
/// Fail-closed on an unresolvable home dir — absence of a readable home must
/// never read as consent.
fn session_identity_marker_present() -> bool {
    // Test-only: let a test drive the operator's switch without touching the
    // developer's real home dir (see [`MarkerOverride`]). Compiled out of the
    // shipped binary entirely.
    #[cfg(test)]
    match MARKER_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    session_identity_marker_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Test-only override of the opt-in-marker stat: `-1` = no override (stat the
/// real marker), `0` = absent, `1` = present.
///
/// Without it, every test of the live-revocation property would silently depend
/// on whether the developer running it happens to have opted THIS machine in —
/// a test that passes for the wrong reason on one box and fails on another.
#[cfg(test)]
static MARKER_OVERRIDE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

/// Serializes the tests that install a [`MARKER_OVERRIDE`], which is
/// process-global.
#[cfg(test)]
static MARKER_OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII handle for [`MARKER_OVERRIDE`]: holds the serialization lock for the
/// test's whole body and restores "no override" on drop, so a panicking test
/// cannot leak the override into the rest of the suite.
#[cfg(test)]
struct MarkerOverride(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl MarkerOverride {
    fn set(present: bool) -> Self {
        let guard = MARKER_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        MARKER_OVERRIDE.store(i8::from(present), std::sync::atomic::Ordering::SeqCst);
        MarkerOverride(guard)
    }

    /// Flip the operator's switch mid-test — this is what makes "deleting the
    /// marker revokes an ALREADY-MINTED nonce" assertable.
    fn flip(&self, present: bool) {
        MARKER_OVERRIDE.store(i8::from(present), std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
impl Drop for MarkerOverride {
    fn drop(&mut self) {
        MARKER_OVERRIDE.store(-1, std::sync::atomic::Ordering::SeqCst);
    }
}

// Re-export of the `.mcp.json` proxy-header contract, which lives in
// `crate::coord_mcp_config` because `coord_doctor` (a LIB module) reads the
// same shape and cannot reach this bin-only module.
pub(crate) use crate::coord_mcp_config::{
    config_doc_has_static_authorization, config_doc_is_agent_marked, proxy_nonce_from_config_doc,
    proxy_nonce_from_header_object, proxy_nonce_from_request, COORD_MCP_PRINCIPAL_AGENT,
    COORD_MCP_PRINCIPAL_HEADER_JSON, COORD_MCP_PROXY_KEY_HEADER, COORD_MCP_PROXY_KEY_HEADER_JSON,
    PROXY_AUTHORIZATION_HEADER_JSON, PROXY_BEARER_PREFIX,
};

/// The lead clause of every loopback-proxy "your key is dead" 401.
///
/// Deliberately does NOT name a header: the old string was
/// `"missing or unrecognized X-Coord-Mcp-Proxy-Key"`, which pointed a 2am
/// reader at the header Phase 2 deprecates, and the key is now accepted under
/// two names anyway. What a reader needs first is WHICH credential died, not
/// which envelope carried it.
pub(crate) const STALE_PROXY_KEY_CAUSE: &str = "stale or unrecognized coord-mcp proxy key: \
     this session's loopback nonce is not registered with the runner currently \
     listening on this port";

/// Same, for the doors that inject the DEVICE bearer and therefore refuse an
/// AGENT-bound nonce outright (claims reads, the coord write forwarder).
pub(crate) const NON_DEVICE_PROXY_KEY_CAUSE: &str =
    "missing, stale, or non-device coord-mcp proxy key: this route injects the \
     device identity, so it serves device-bound nonces only";

/// The `AGENT_TOKENS`-slot-gone variant: the nonce IS registered, but the agent
/// it is bound to no longer has a live token slot.
pub(crate) const AGENT_GONE_PROXY_CAUSE: &str =
    "no live agent token for this proxy session: the agent this nonce is bound \
     to has been torn down, or the runner restarted (agent tokens are \
     process-global and are NOT restored across a restart)";

/// The shared, verified recovery tail — the half a human actually acts on.
///
/// Names the thing that makes this failure feel unrecoverable: the MCP client
/// snapshots its headers at launch and never re-reads `.mcp.json`, so the
/// obvious move (reconnect the server) cannot work. Measured 2026-08-20 at
/// client 2.1.236/2.1.237 (plan
/// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`, Phase 1). Every
/// step below is one Phase 5 **verified** end-to-end on an isolated
/// `CLAUDE_CONFIG_DIR`; nothing here is inferred.
///
/// **What this string must never do is name an unverified repair** — that is
/// the defect class the whole plan exists to retire. Two candidates were
/// deliberately cut for exactly that reason:
///
/// * `POST /coord-mcp/provision-session` — it reads as the obvious recovery and
///   is not one. The route is gated by [`session_identity_gate`]: the caller
///   must present this runner start's owner-only loopback handshake key AND the
///   machine must carry the opt-in marker, so on an un-opted machine the advised
///   recovery is a **denial**; and even when both hold it mints an EPHEMERAL,
///   `terminal_id: None`, never-persisted nonce — a strictly weaker credential
///   class than the persistent per-terminal one that just died, gone again at
///   the next restart, and revocable mid-session by deleting the marker.
/// * Restarting the runner — forbidden outright (served policy
///   `production-and-cost` `runner-lifecycle`), and it orphans every OTHER
///   session's key, which is the incident this plan was written from.
pub(crate) const PROXY_KEY_RECOVERY_HINT: &str = "The MCP client snapshots its headers at \
     launch and never re-reads .mcp.json, so reconnecting this server cannot pick up a fresh \
     key. Recovery, in order: (1) keep working through another coord door (/coord-revive), \
     and verify any write by reading it back; (2) start a NEW session in this workdir — the \
     runner writes a fresh key on every session spawn; NEVER restart the runner to force it; \
     (3) only if the client reports needs-auth while sending ZERO requests, run `claude mcp \
     logout <server>`, then start a new session. The key is accepted as \
     `Authorization: Bearer <nonce>` or the legacy `X-Coord-Mcp-Proxy-Key`.";

/// Join a door-specific cause with the shared recovery tail. One function so
/// the five proxy doors cannot drift into five different 2am stories.
pub(crate) fn stale_proxy_key_error(cause: &str) -> String {
    format!("{cause}. {PROXY_KEY_RECOVERY_HINT}")
}

/// Which side of the coord-mcp proxy hop a failure came from (plan
/// `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`, Phase 3b).
///
/// **This distinction is the whole point.** A runner-nonce refusal and a coord
/// rejection of the forwarded bearer both arrive at the caller as a 401 with a
/// short body, and nothing in the wire shape separates them — which is why
/// `/coord-revive`'s `classify()` maps *every* bare 401 onto the runner-nonce
/// story ("stale/evicted proxy key") and reports the coord-upstream class as
/// something it is not. The two have OPPOSITE recoveries: a dead nonce is fixed
/// by starting a new session, while a dead upstream bearer follows that session
/// into the new one and is fixed only by re-minting the device credential.
///
/// The proxy is the ONLY place on this box that can see both sides of the hop,
/// so it is the only place that can name the layer honestly. Every consumer
/// downstream is guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxyFailureLayer {
    /// The runner refused BEFORE forwarding: the loopback nonce is absent,
    /// stale, evicted, or bound to a principal this door does not serve. Coord
    /// was never dialed and is not implicated.
    RunnerNonce,
    /// The runner forwarded and COORD refused the injected bearer. The nonce
    /// was fine — a new session will mint a new nonce and fail identically.
    CoordUpstream,
    /// The hop could not be completed at all (DNS, connect, TLS, timeout, an
    /// unreadable body, a gateway error page). This is UNKNOWN about BOTH
    /// credentials, never a rejection of either — the distinction
    /// `verification-and-evidence` `silent-empty-is-unknown` exists to keep.
    RunnerTransport,
}

impl ProxyFailureLayer {
    /// The stable machine token. Kept kebab-case to match the rotation-log
    /// vocabulary (`reject`, `coord-rejection`) rather than the JSON style, so
    /// one grep spans the log and the envelope.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProxyFailureLayer::RunnerNonce => "runner-nonce",
            ProxyFailureLayer::CoordUpstream => "coord-upstream",
            ProxyFailureLayer::RunnerTransport => "runner-transport",
        }
    }

    /// Where the caller should go NEXT, given this layer — the field that lets
    /// a session recover without consulting a document (dossier `c789d751`
    /// direction 1: "make the silence itself carry the answer").
    ///
    /// Each string names a door that is actually reachable from a session whose
    /// loopback transport just failed. None of them says "restart the runner":
    /// that is forbidden outright (served policy `production-and-cost`
    /// `runner-lifecycle`) and it orphans every OTHER session's key.
    pub(crate) fn next_door(self) -> &'static str {
        match self {
            // The nonce is the dead part, and it is re-minted per session spawn.
            ProxyFailureLayer::RunnerNonce => {
                "Start a NEW session in this workdir — the runner writes a fresh key on every \
                 session spawn. Meanwhile POST $COORD_HTTP_URL/mcp (JSON-RPC, no session \
                 handshake) with a device JWT: it does not traverse this proxy, so a dead \
                 nonce cannot reach it."
            }
            // A new session re-mints the NONCE, not the bearer — so the usual
            // advice is exactly the wrong one here, and saying so is the value.
            ProxyFailureLayer::CoordUpstream => {
                // Deliberately does NOT claim the refresher was kicked. That is
                // true only for a DEVICE principal; an AGENT bearer comes from
                // that agent's own token slot and this proxy never touches the
                // device refresher for it. Whether a retry actually happened is
                // reported per-response in the `retry` field, which is measured
                // — putting it here would make a constant assert something no
                // code path established, the defect class this plan exists to
                // retire.
                "The loopback nonce is FINE — starting a new session will NOT help, because the \
                 bearer this proxy injects is not the nonce and does not change with the \
                 session. See the `retry` field for what this runner already attempted. For \
                 the selected slot's kid/exp and which door on this box is live, GET \
                 /coord-mcp/doctor."
            }
            // Naming a credential door here would assert a cause nothing tested.
            ProxyFailureLayer::RunnerTransport => {
                "Neither credential is implicated — the hop itself did not complete. Re-try; if \
                 it persists, check coord's own reachability (GET $COORD_HTTP_URL/health) before \
                 touching any credential."
            }
        }
    }
}

/// Build the typed failure envelope every coord-mcp proxy failure returns
/// (Phase 3b).
///
/// `error` and `code` are passed through **verbatim** by every caller, so this
/// is purely ADDITIVE: any consumer still matching on the prose or the code
/// keeps working byte-for-byte, and the new `layer` / `cause` / `next_door` /
/// `probed_at` fields are what a consumer moves onto. That matters because the
/// door this is meant to fix — `/coord-revive` — must be able to distinguish
/// the two classes **by body** on a runner that predates this change as well as
/// one that carries it.
///
/// `probed_at` is the wall-clock of THIS hop, not of anything cached. It is the
/// field that stops a durable artifact asserting unavailability without saying
/// when it learned that (dossier `c632da1c`, `stale-capability-floor`).
pub(crate) fn proxy_failure_envelope(
    error: impl Into<String>,
    code: &str,
    layer: ProxyFailureLayer,
    cause: impl Into<String>,
    extra: &[(&str, serde_json::Value)],
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "success": false,
        "error": error.into(),
        "code": code,
        "layer": layer.as_str(),
        "cause": cause.into(),
        "next_door": layer.next_door(),
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(obj) = v.as_object_mut() {
        for (k, val) in extra {
            obj.insert((*k).to_string(), val.clone());
        }
    }
    v
}

/// Normalize a workdir on its way into a [`NonceBinding`] (Phase 3c).
///
/// **Two sentinels are one sentinel too many.** Of 1,049 `reject` rows measured
/// on the operator box 2026-08-31, 849 carried `""` and 200 carried
/// `"unknown"` — so a reader filtering the honest sentinel still missed
/// four-fifths of the unattributable rows, and a reader counting `""` as "no
/// workdir field" missed the rest. `RejectAttribution`'s doc already promised
/// the field is "never left empty"; the promise was kept only on the lookup-MISS
/// path, never on a live binding that was registered with an empty string.
///
/// This is the constructor-side half: an empty or whitespace-only workdir
/// becomes [`ROTATION_UNKNOWN`] before it can enter the map, so the two
/// sentinels collapse into the one that says what it means. It deliberately
/// does NOT invent a workdir — a binding registered without one genuinely has
/// none, and `unknown` is the honest rendering
/// (`verification-and-evidence` `unknown-must-not-render-as-a-default`).
fn normalize_binding_workdir(workdir: &str) -> String {
    if workdir.trim().is_empty() {
        ROTATION_UNKNOWN.to_string()
    } else {
        workdir.to_string()
    }
}

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
///
/// # There is deliberately no source-dropping companion
///
/// This module used to carry `coord_base_url()` and `coord_mcp_url()`, whose
/// entire bodies were `.0` on the `_with_source` form. Phase 2 of
/// `2026-08-20-effective-config-provenance-and-env-generation` deleted both.
/// A wrapper like that is not a convenience — it is a discard LAYER: every one
/// of its call sites drops the arm invisibly, so nothing at the call site, and
/// nothing in a grep for `.0`, shows that provenance was thrown away. The
/// eleven call sites now bind `(base, _coord_base_source)` and the discard is
/// legible where it happens. Do not reintroduce a `.0` wrapper here.
pub(crate) fn coord_base_url_with_source(
) -> (String, qontinui_runner_lib::profiles::CoordBaseSource) {
    let (base, source) = qontinui_runner_lib::profiles::coord_base_with_source();
    (base.trim_end_matches('/').to_string(), source)
}

/// The full coord `/mcp` endpoint URL + source: [`coord_base_url_with_source`]
/// with `/mcp` appended. Shared by the static-bearer `.mcp.json` writer (agent
/// path) and the loopback proxy forwarder (`mcp_api::coord_mcp_proxy_handler`).
pub(crate) fn coord_mcp_url_with_source() -> (String, qontinui_runner_lib::profiles::CoordBaseSource)
{
    let (base, source) = coord_base_url_with_source();
    (format!("{base}/mcp"), source)
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

/// Snapshot every registered slot's refresh health, newest problem first.
///
/// Two-phase by necessity: the registry is behind a `std::sync::Mutex` but
/// each slot is behind a `tokio::sync::RwLock`, so the `Arc`s are cloned out
/// and the registry lock released **before** any `.await`. Holding a blocking
/// mutex across an await point is how an executor deadlocks.
///
/// Ordering puts `Rejected` first, then `Degraded`, then healthy — a reader
/// scanning the head of the list sees the problems without paging.
pub(crate) async fn agent_token_health_snapshot() -> Vec<crate::agent_token::AgentTokenHealth> {
    let slots: Vec<(Uuid, crate::agent_token::SharedToken)> = {
        agent_tokens()
            .lock()
            .expect("agent token map poisoned")
            .iter()
            .map(|(id, slot)| (*id, slot.clone()))
            .collect()
    };
    let now = chrono::Utc::now().timestamp();
    let mut out = Vec::with_capacity(slots.len());
    for (agent_id, slot) in slots {
        out.push(slot.read().await.health_report(agent_id, now));
    }
    out.sort_by(|a, b| {
        use crate::agent_token::TokenState::*;
        let rank = |s| match s {
            Rejected => 0,
            Degraded => 1,
            Healthy => 2,
        };
        rank(a.state)
            .cmp(&rank(b.state))
            .then(b.consecutive_failures.cmp(&a.consecutive_failures))
            .then(a.agent_id.cmp(&b.agent_id))
    });
    out
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
///
/// Set ONLY on a call that actually reached the restore — a
/// persistence-disabled call must not burn it (see
/// [`PROXY_NONCES_RESTORE_DISABLED_LOGGED`]).
static PROXY_NONCES_RESTORED: OnceLock<()> = OnceLock::new();

/// One-shot for the persistence-DISABLED arm's aggregate `restore` line, held
/// separately from [`PROXY_NONCES_RESTORED`] so the two properties do not fight:
/// the log stays one-line-per-process-per-outcome, while a disabled call leaves
/// the actual restore still available to a later enabled one.
static PROXY_NONCES_RESTORE_DISABLED_LOGGED: OnceLock<()> = OnceLock::new();

fn proxy_nonces() -> &'static Mutex<HashMap<String, NonceBinding>> {
    PROXY_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The AGENT arm of the grace-TTL split (plan
/// 2026-07-27-coord-mcp-flake-remediation, Phase 5/R3): the pre-split 90s
/// bound retained under its own name so the device-arm widening below can
/// never silently leak into the agent class. Deliberately UNUSED by the grace
/// path — agent nonces are never graced AT ALL (they hard-fail closed on
/// re-mint/restart, the scope-elevation non-goal, OQ3) — it exists as the
/// named ceiling any future agent-class grace must consciously adopt, and the
/// grace tests pin it against [`DEVICE_EVICTED_NONCE_GRACE_TTL`].
#[cfg_attr(not(test), allow(dead_code))] // documented ceiling; consumed only by the grace tests
const AGENT_NONCE_GRACE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// Grace window (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart,
/// Change 3, defense in depth): a DEVICE nonce evicted by a same-workdir re-mint
/// stays valid for this long so an in-flight MCP client that cached it rides
/// through until it reconnects and re-reads the freshly-written `.mcp.json` (the
/// client never re-reads the file mid-connection, so a hard eviction 401s it the
/// instant the file is rewritten). Bounded — the accept-set widening lasts only
/// this window, and only for a device nonce the runner itself just superseded.
/// AGENT nonces are NEVER graced: they must hard-fail closed on re-mint/restart
/// (the scope-elevation non-goal, OQ3).
///
/// Widened 90s → 6h by plan 2026-07-27-coord-mcp-flake-remediation (Phase
/// 5/R3): the persistent-nonce class is one-slot-per-workdir, so ANY peer
/// session/terminal opened with the same cwd re-provisions `.mcp.json` and
/// evicts the live peer's key — and since the MCP client never re-reads the
/// file, 90s of grace meant time-to-transport-death ≈ time-to-first-peer
/// -re-provision (the fleet-wide "Command failed with no output" flake, 11+
/// sessions). 6h covers a working session's lifetime while staying bounded:
/// the widened accept window is loopback-only, device-class, PERSISTENT-class
/// only (ephemeral nonces never grace — grace would bypass the opt-out kill
/// switch, which only [`live_binding`] enforces), and is entered from exactly
/// two runner-initiated paths: a same-workdir re-mint superseding the key
/// ([`mint_and_register_nonce`]) and the close-time eviction of a per-session
/// workdir ([`evict_proxy_nonces_for_workdir`] — a straggling in-flight client
/// of a just-closed session rides the same window). The one-slot eviction
/// invariant is untouched (the old key still dies — deterministically, just
/// later). Operator-tunable by editing this const.
const DEVICE_EVICTED_NONCE_GRACE_TTL: std::time::Duration =
    std::time::Duration::from_secs(6 * 60 * 60);

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
/// [`DEVICE_EVICTED_NONCE_GRACE_TTL`] expiry, opportunistically pruning expired
/// entries so the map stays bounded. Only device nonces are passed here (the
/// caller filters); agent nonces are dropped outright to fail closed.
fn grace_evicted_device_nonces(nonces: &[String]) {
    if nonces.is_empty() {
        return;
    }
    let now = std::time::Instant::now();
    let expires_at = now + DEVICE_EVICTED_NONCE_GRACE_TTL;
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

/// The sentinel every attribution field carries when the fact is genuinely not
/// knowable at emission time — an unregistered/evicted nonce has no binding
/// left to read. Spelled out rather than left empty because an empty string
/// reads as "the runner did not populate this field", which is the ambiguity
/// that made the 2026-08-19 reject lines unattributable (all 671 of them
/// carried `"workdir":""` and nobody could tell absence from ignorance).
const ROTATION_UNKNOWN: &str = "unknown";

/// Which runner instance emitted a line. `None` (the primary) is spelled
/// `"primary"` so every line carries a positive value; secondaries carry their
/// `QONTINUI_INSTANCE_NAME`. Paired with the pid this disambiguates lines from
/// two runners sharing one dev-logs dir, and — because the pid changes on every
/// restart — makes the restart boundary itself readable off the stream.
fn rotation_runner_id() -> String {
    crate::instance::instance_name().unwrap_or_else(|| "primary".to_string())
}

/// Build one rotation-forensics JSONL line. Pure over its inputs (bar the
/// timestamp, the runner id and the pid) so the shape — and the prefix-only
/// guarantee — is unit-testable without touching the filesystem.
///
/// `extra` carries the per-event attribution fields the base shape has no room
/// for (`principal` / `terminal_id` on a `reject`, the counts on a `restore`).
/// It is merged into the top-level object rather than nested so a `jq` filter
/// over the stream stays flat.
fn rotation_log_line_with(
    event: &str,
    workdir: &str,
    nonce: &str,
    cause: &str,
    extra: &[(&str, serde_json::Value)],
) -> String {
    let mut line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "workdir": workdir,
        "key_prefix": rotation_key_prefix(nonce),
        "cause": cause,
        // Phase 3 (plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning):
        // WHICH runner process wrote this. Secondary runners scope their own
        // dev-logs dir, but a settings override can point several at one dir,
        // and the pid is the only field that changes across a restart of the
        // SAME instance — which is precisely the boundary the incident
        // reconstruction had to infer from timestamps.
        "runner_id": rotation_runner_id(),
        "pid": std::process::id(),
    });
    if let Some(obj) = line.as_object_mut() {
        for (k, v) in extra {
            obj.insert((*k).to_string(), v.clone());
        }
    }
    line.to_string()
}

/// [`rotation_log_line_with`] for the events that carry no extra attribution.
fn rotation_log_line(event: &str, workdir: &str, nonce: &str, cause: &str) -> String {
    rotation_log_line_with(event, workdir, nonce, cause, &[])
}

/// Shared `cause` text for a "grace" forensics line, naming the active TTL so
/// the log self-documents how long the evicted key stays acceptable.
fn rotation_grace_cause() -> String {
    format!(
        "evicted device nonce graced {}s",
        DEVICE_EVICTED_NONCE_GRACE_TTL.as_secs()
    )
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
    log_rotation_event_with(event, workdir, nonce, cause, &[]);
}

/// [`log_rotation_event`] carrying extra top-level attribution fields — the
/// `principal` / `terminal_id` a `reject` can resolve, or the counts a
/// `restore` reports. Same best-effort, same lock discipline, same one
/// `write_all` per line.
fn log_rotation_event_with(
    event: &str,
    workdir: &str,
    nonce: &str,
    cause: &str,
    extra: &[(&str, serde_json::Value)],
) {
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
        let mut line = rotation_log_line_with(event, workdir, nonce, cause, extra);
        line.push('\n');
        let _ = f.write_all(line.as_bytes());
    }
}

/// Record that a per-tenant device-JWT slot was CLEARED, on the same
/// rotation-forensics JSONL the nonce lifecycle uses.
///
/// Plan `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
/// Phase 2: clearing a credential is the one destructive act the refresher
/// performs, so every clear names its evidence. `cause` is the evidence CLASS
/// (`decoded-expiry` / `coord-rejection` — the only two that may reach a
/// clear); `evidence` is the concrete observation (the decoded `exp`, or the
/// coord status).
///
/// The `workdir` column has no meaning for a credential event, so it carries a
/// positive sentinel rather than an empty string — the 2026-08-19 incident
/// turned on 671 rows whose empty `workdir` could not be told from an
/// unpopulated one. `key_prefix` is empty for the same reason it is truncated
/// elsewhere: the slot is named by its tenant, and no key material belongs on
/// this stream.
pub(crate) fn log_device_jwt_slot_clear(tenant: &Uuid, cause: &str, evidence: &str) {
    log_rotation_event_with(
        "clear-device-jwt-slot",
        CREDENTIAL_STORE_WORKDIR,
        "",
        evidence,
        &[
            ("tenant_id", serde_json::json!(tenant.to_string())),
            ("slot", serde_json::json!(format!("device_jwt:{tenant}"))),
            ("clear_cause", serde_json::json!(cause)),
        ],
    );
}

/// The `workdir` sentinel for rotation rows that describe the credential store
/// rather than a session workdir.
const CREDENTIAL_STORE_WORKDIR: &str = "(credential-store)";

/// The resolved absolute path of the rotation-forensics JSONL, or `None` when
/// file emission is off (the test default).
///
/// Exists because the 2026-08-19 investigation searched `D:/qontinui-root` and
/// `C:/claude` for this file, concluded it "does not exist", and wrote off its
/// single best evidence source — while the file sat under `%LOCALAPPDATA%`
/// with 5,486 lines of the incident in it. [`rotation_log_dir`] resolves
/// through [`crate::paths::get_dev_logs_dir`] (settings override → app-data
/// default → instance-scoped), so the path is not guessable from the repo.
/// Nobody should have to guess it again: it is logged once at boot and served
/// on `/health`.
pub(crate) fn rotation_log_path() -> Option<std::path::PathBuf> {
    rotation_log_dir().map(|d| d.join(ROTATION_LOG_FILE))
}

/// Emitted-once boot breadcrumb naming [`rotation_log_path`] at INFO. Called
/// from the boot task alongside the nonce restore. Idempotent: a duplicate
/// boot-task run logs nothing further.
static ROTATION_LOG_PATH_LOGGED: OnceLock<()> = OnceLock::new();

pub(crate) fn log_rotation_log_path_once() {
    if ROTATION_LOG_PATH_LOGGED.set(()).is_err() {
        return;
    }
    match rotation_log_path() {
        Some(p) => info!(
            "coord_mcp: rotation forensics log = {} (NOT in the workspace — \
             resolved via paths::get_dev_logs_dir)",
            p.display()
        ),
        None => info!("coord_mcp: rotation forensics log disabled (no dev-logs dir resolved)"),
    }
}

/// The `/health` view of the rotation forensics stream: where it is and
/// whether it is there. Two `null`s means file emission is off.
pub(crate) fn rotation_log_health_json() -> serde_json::Value {
    match rotation_log_path() {
        Some(p) => serde_json::json!({
            "path": p.to_string_lossy(),
            "exists": p.try_exists().unwrap_or(false),
        }),
        None => serde_json::json!({ "path": serde_json::Value::Null, "exists": false }),
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

/// What a `reject` line can say about WHOSE key just died. Every field is
/// populated or explicitly [`ROTATION_UNKNOWN`] — never left empty, so a reader
/// can tell "the runner does not know" from "the runner did not fill this in".
struct RejectAttribution {
    /// The bound workdir, or [`ROTATION_UNKNOWN`].
    workdir: String,
    /// `"device"` / `"agent"` / [`ROTATION_UNKNOWN`].
    principal: String,
    /// The bound terminal, [`ROTATION_UNKNOWN`] for an unknown nonce, or
    /// `"none"` for a live binding that legitimately has no terminal (restored,
    /// adopted, mint-route, agent, in-cwd writer) — a real, distinct fact.
    terminal_id: String,
}

/// Resolve everything a rejected nonce can still be attributed to, read WITHOUT
/// mutating either registry — unlike [`live_binding`], which lazily evicts an
/// expired ephemeral as a side effect. The reject forensics line runs on the
/// request path after the gate has already decided, so it must not change
/// registry state.
///
/// Two sources, live registry first then the grace map, because those are the
/// two places a nonce the handler just saw can still be known. A graced nonce
/// has no binding left (grace is keyed by nonce alone), so it can name its
/// principal — always DEVICE, grace is device-only — and nothing else.
///
/// **Neither lock is held on return**, which is the point: the caller feeds
/// this into [`log_rotation_event_with`], which does file I/O, and
/// `log_rotation_event` documents that callers must not hold the registry lock
/// across it. The clones are the price of that discipline.
fn reject_attribution_for_nonce(nonce: &str) -> RejectAttribution {
    let unknown = || RejectAttribution {
        workdir: ROTATION_UNKNOWN.to_string(),
        principal: ROTATION_UNKNOWN.to_string(),
        terminal_id: ROTATION_UNKNOWN.to_string(),
    };
    if nonce.is_empty() {
        return unknown();
    }
    let live = {
        let map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.get(nonce).map(|b| {
            (
                b.workdir.clone(),
                match b.principal {
                    ProxyPrincipal::Device => "device".to_string(),
                    ProxyPrincipal::Agent { .. } => "agent".to_string(),
                },
                b.terminal_id.clone(),
            )
        })
    };
    if let Some((workdir, principal, terminal_id)) = live {
        return RejectAttribution {
            // Phase 3c, read side. All three construction sites normalize, so
            // this is belt-and-braces — but it is what actually makes this
            // struct's doc ("never left empty") TRUE for every future
            // construction site as well as today's three, and it is the one
            // place every `reject` row provably passes through.
            workdir: normalize_binding_workdir(&workdir),
            principal,
            terminal_id: terminal_id.unwrap_or_else(|| "none".to_string()),
        };
    }
    // Grace map fallback: only DEVICE nonces are ever graced, so a hit here
    // pins the principal class even though the binding itself is gone.
    if graced_nonces()
        .lock()
        .expect("graced nonce map poisoned")
        .contains_key(nonce)
    {
        return RejectAttribution {
            workdir: ROTATION_UNKNOWN.to_string(),
            principal: "device".to_string(),
            terminal_id: ROTATION_UNKNOWN.to_string(),
        };
    }
    unknown()
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
    // name its own workdir, principal and terminal; an unregistered or evicted
    // one cannot — that is what the prefix join, and the explicit
    // `"unknown"`s, are for. Resolved WITHOUT holding either registry lock
    // across the file I/O below.
    let attr = reject_attribution_for_nonce(nonce);
    let cause = if suppressed > 0 {
        format!("{cause} [+{suppressed} identical rejects suppressed since the previous line]")
    } else {
        cause.to_string()
    };
    log_rotation_event_with(
        "reject",
        &attr.workdir,
        nonce,
        &cause,
        &[
            ("principal", serde_json::Value::from(attr.principal)),
            ("terminal_id", serde_json::Value::from(attr.terminal_id)),
        ],
    );
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
    spawn_blocking_tracked(move || log_proxy_nonce_rejected(nonce.as_deref(), &cause));
}

/// Record a coord-mcp proxy request that the runner FORWARDED and **coord**
/// rejected (Phase 3c) — the other half of the story `reject` tells.
///
/// A separate event name, not a `reject` row with a different cause string.
/// `reject` means "the runner refused this nonce"; every consumer of the
/// rotation trail reads it that way, and folding an upstream rejection into it
/// would make the runner's own refusal count unusable — the exact
/// mis-attribution [`ProxyFailureLayer`] exists to prevent, reproduced in the
/// log instead of in the response.
///
/// The nonce is still the join key: it is live (it passed the gate, or we would
/// never have forwarded), so [`reject_attribution_for_nonce`] resolves a REAL
/// workdir here — which is precisely the attribution the `reject` rows usually
/// cannot supply, and why this row is worth emitting separately.
pub(crate) fn log_proxy_upstream_rejected(nonce: Option<&str>, status: u16, cause: &str) {
    let nonce = nonce.unwrap_or("");
    // NAMESPACED, and that is load-bearing. `reject_throttle_admit` is keyed by
    // the key prefix alone, so sharing it would let a runner-nonce `reject` and
    // a coord `upstream-reject` for the SAME nonce silently suppress each other
    // inside one window — re-creating, inside the throttle, exactly the
    // conflation of the two layers this phase exists to end. `reject`'s own key
    // is left byte-identical so its behaviour and its test are unchanged.
    let prefix = rotation_key_prefix(nonce);
    let Some(suppressed) = reject_throttle_admit(&format!("upstream:{prefix}")) else {
        return;
    };
    let attr = reject_attribution_for_nonce(nonce);
    let cause = if suppressed > 0 {
        format!("{cause} [+{suppressed} identical rejects suppressed since the previous line]")
    } else {
        cause.to_string()
    };
    log_rotation_event_with(
        "upstream-reject",
        &attr.workdir,
        nonce,
        &cause,
        &[
            ("principal", serde_json::Value::from(attr.principal)),
            ("terminal_id", serde_json::Value::from(attr.terminal_id)),
            ("upstream_status", serde_json::Value::from(status)),
            (
                "layer",
                serde_json::Value::from(ProxyFailureLayer::CoordUpstream.as_str()),
            ),
        ],
    );
}

/// [`log_proxy_upstream_rejected`] for an ASYNC caller — same detached,
/// fire-and-forget contract as [`spawn_log_proxy_nonce_rejected`]: a proxied
/// response must never wait on forensics.
pub(crate) fn spawn_log_proxy_upstream_rejected(
    nonce: Option<&str>,
    status: u16,
    cause: impl Into<String>,
) {
    let nonce = nonce.map(str::to_owned);
    let cause = cause.into();
    tokio::task::spawn_blocking(move || {
        log_proxy_upstream_rejected(nonce.as_deref(), status, &cause)
    });
}

/// Project the live nonce map down to the DEVICE-only shape the encrypted store
/// persists (OQ3): agent bindings are dropped so they never reach disk.
///
/// **This function IS the persisted shape.** The whole chain is typed on its
/// return value — `device_nonce_snapshot` → [`enqueue_nonce_persist`] /
/// [`persist_proxy_nonces`] →
/// [`crate::secure_storage::SecureStorage::store_coord_mcp_nonces`] →
/// `StoredTokens::coord_mcp_nonces` →
/// [`crate::secure_storage::SecureStorage::load_coord_mcp_nonces`] →
/// [`restore_proxy_nonces_from`]. Widening the store and the restore leg alone
/// would ship inert: `terminal_id` would never be WRITTEN, so every restore
/// would still land `None` and the slot-collapse would survive untouched.
///
/// Widened to carry `terminal_id` by plan 2026-08-20 Phase 4 — see
/// [`crate::secure_storage::StoredNonceBinding`] for why, and for what is
/// deliberately still not carried.
///
/// **Both existing drops are preserved verbatim** and are not up for
/// relitigation here:
/// - `principal == Device` (OQ3) — agent nonces must never reach disk.
/// - [`NonceLifetime::Ephemeral`] (plan 2026-07-17 §1/E) — a mint-route nonce is
///   issued to a session the runner did not spawn, so it must not outlive this
///   process: the store has no expiry column, so a persisted ephemeral nonce
///   would silently restore as an UNBOUNDED one, laundering the weaker class
///   into the stronger one across a restart. Non-persistence is also half of
///   what makes the TTL meaningful (a leaked nonce cannot be replayed against
///   the next runner).
///
/// ## The bound — the third drop, and why it has to live here
///
/// Phase 4 carried `terminal_id` into the store, which fixed the slot-collapse
/// cascade but removed the only thing that was reaping the persisted set. It
/// was an accidental GC and a destructive one — that is the bug Phase 4 fixed —
/// but what took its place reaches **less than all of it**:
/// `revoke_proxy_nonce` has no production caller (every reference is a test),
/// `evict_proxy_nonces_for_workdir` fires only from `mcp/backend_relay.rs` on
/// relay-chat close for a per-session relay workdir, `terminal/session.rs`
/// provisions on spawn and revokes nothing on close, and a persistent binding
/// is otherwise evicted only by a re-mint for its own `(workdir, terminal_id)`
/// slot — and terminal ids are per-PTY, so that slot is never reused.
///
/// The one production reaper is [`release_workdir_on_session_close`], via
/// `session_lifecycle_store::record_close`: when the LAST open record for a
/// workdir closes it drops every nonce bound to that workdir and mirrors the
/// shrunken set. That is real coverage and this doc used to deny it. What it
/// does NOT cover is the accumulator that actually matters:
///
/// * **the `qontinui-root` workdir, which that reaper returns early on by
///   design** (its nonce is the shared credential for every root-launched
///   session, healed at boot rather than per-session). Root is also where the
///   most terminals are spawned, and each gets its own per-PTY slot — so root
///   alone still adds one permanent entry per terminal, forever;
/// * a session that never reaches `record_close` at all — a runner killed
///   rather than closed, a terminal with no lifecycle record.
///
/// Both are restored into the live map at every boot, with no expiry column,
/// in an encrypted store that is fully re-encrypted and rewritten on every
/// mint. The growth is narrower than "every terminal ever spawned" but it is
/// still monotone and still unbounded, which is what the cap below answers.
///
/// The bound is applied HERE, at the snapshot, rather than by revoking on
/// terminal close, and that choice is the point:
///
/// * The **live map stays authoritative** for this process. Capping the
///   snapshot cannot 401 a running session — it only decides which bindings are
///   still there after the NEXT restart. Revoking on terminal close would kill
///   a live credential, and the in-cwd `.mcp.json` that terminal wrote is read
///   by other consumers (the `qontinui-pr` walk-up, hand-launched clients), so
///   that shape can strand a session that is still using the file.
/// * Eviction is **oldest-first** ([`NonceBinding::minted_at`]), tie-broken by
///   the nonce string so the emitted map is deterministic — `enqueue_nonce_persist`
///   compares snapshots for equality to skip no-op writes, and a
///   nondeterministic cut would rewrite the encrypted store on every mint.
///   The age is itself persisted
///   ([`crate::secure_storage::StoredNonceBinding::minted_at_unix`]), which is
///   what makes "oldest" mean anything after a restart: stamping restored
///   bindings with the restore instant made them all tie, so at the exact
///   moment the cap first engaged the cut degenerated into a random pick over
///   hex nonce strings — able to drop a live session's credential in favour of
///   a long-dead terminal's.
/// * It is the whole persisted surface: `device_nonce_snapshot` is the single
///   producer of the stored shape, so nothing can route around the bound.
///
/// What it does NOT bound is the in-memory map within one process lifetime.
/// That is deliberate: those are credentials issued to PTYs this process
/// actually spawned, and dropping one mid-life is the stranding case above. The
/// bound is on what becomes *eternal*.
fn device_nonce_snapshot(
    map: &HashMap<String, NonceBinding>,
) -> HashMap<String, crate::secure_storage::StoredNonceBinding> {
    let mut eligible: Vec<(&String, &NonceBinding)> = map
        .iter()
        .filter(|(_, b)| b.principal == ProxyPrincipal::Device && !b.lifetime.is_ephemeral())
        .collect();
    if eligible.len() > MAX_PERSISTED_DEVICE_NONCES {
        // Newest first, then keep the head. Ages are real — a restored binding
        // carries its PERSISTED mint time, not the restore instant — so this
        // orders by true age across restarts. The `nonce` tiebreak makes the
        // cut total and deterministic for the residual ties: two mints inside
        // one second, and the unknown-age entries a pre-timestamp store
        // restores at `UNIX_EPOCH` (which tie at the OLDEST end, so they are
        // cut before any dated binding).
        eligible.sort_by(|(na, a), (nb, b)| b.minted_at.cmp(&a.minted_at).then_with(|| na.cmp(nb)));
        let dropped = eligible.len() - MAX_PERSISTED_DEVICE_NONCES;
        eligible.truncate(MAX_PERSISTED_DEVICE_NONCES);
        warn!(
            "coord_mcp: persisted device nonce set capped at {MAX_PERSISTED_DEVICE_NONCES} \
             — dropped the {dropped} oldest binding(s) from the encrypted store. They stay \
             VALID in this process; they will simply not be restored after the next restart."
        );
    }
    eligible
        .into_iter()
        .map(|(n, b)| {
            (
                n.clone(),
                crate::secure_storage::StoredNonceBinding {
                    workdir: b.workdir.clone(),
                    terminal_id: b.terminal_id.clone(),
                    // The age goes to disk, so the next boot's eviction order is
                    // by TRUE age instead of a restore-instant tie. An
                    // unknown-age binding (restored at `UNIX_EPOCH`) re-emits as
                    // `Some(0)`, which reads back as the same sentinel — unknown
                    // stays unknown-and-oldest, never laundered into "just now".
                    minted_at_unix: Some(minted_at_to_unix(b.minted_at)),
                },
            )
        })
        .collect()
}

/// How many device bindings [`device_nonce_snapshot`] will persist. See that
/// function for why the bound lives at the snapshot rather than at revoke time.
///
/// **The bound is against CUMULATIVE terminal spawns, not the concurrent
/// working set.** One production path does reap the persisted set —
/// [`release_workdir_on_session_close`], on the last open record for a workdir
/// closing — but it returns early on the `qontinui-root` workdir by design, and
/// never runs for a session that dies without a `record_close`. Root is where
/// the most terminals are spawned and every one takes its own per-PTY
/// `(workdir, terminal_id)` slot, so root alone still adds one entry that lives
/// forever. Sizing this against
/// concurrency (~9 sessions on the heaviest measured box) would be sizing
/// against the wrong quantity: at that rate 256 CUMULATIVE spawns is weeks, not
/// never. **An operator on a long-lived install WILL meet this cap.**
///
/// What the cap guarantees, given persisted ages
/// ([`crate::secure_storage::StoredNonceBinding::minted_at_unix`]): what it
/// drops is the genuinely oldest — bindings minted furthest in the past, which
/// on a box where sessions come and go are overwhelmingly dead terminals — and
/// never a newer binding in favour of an older one. It cannot 401 anything in
/// this process (the live map is untouched); it only decides what is still
/// there after the NEXT restart. Without persisted ages this same cut was a
/// coin flip over hex strings that could drop a live session's credential to
/// keep a year-old dead terminal's, which is the orphaning the plan is about.
///
/// The size itself buys headroom, not immunity: 256 × ~150 B ≈ 38 KB of nonce
/// entries in a store that was 9 KB before any of this, re-encrypted and
/// rewritten on every mint. Higher costs store size and rewrite cost on a hot
/// path for bindings that are almost all dead; lower shortens the time to the
/// first eviction of something still useful.
const MAX_PERSISTED_DEVICE_NONCES: usize = 256;

/// [`NonceBinding::minted_at`] → the persisted form: whole seconds since the
/// Unix epoch (see
/// [`crate::secure_storage::StoredNonceBinding::minted_at_unix`] for why that
/// form). A clock reading before the epoch — only reachable via a badly wrong
/// system clock — collapses to `0`, i.e. "unknown, therefore oldest", rather
/// than panicking on the persist path.
fn minted_at_to_unix(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The persisted age → [`NonceBinding::minted_at`]. `None` (an entry written
/// before the field existed, or a pre-Phase-4 bare string) becomes
/// `UNIX_EPOCH`, which sorts OLDEST — the honest reading of "age unrecoverable,
/// but certainly older than anything a timestamp-writing binary minted". A
/// value so large it overflows `SystemTime` (corrupt store) lands on the same
/// sentinel rather than panicking on the boot path.
fn minted_at_from_unix(secs: Option<u64>) -> std::time::SystemTime {
    match secs {
        Some(s) => std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(s))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        None => std::time::SystemTime::UNIX_EPOCH,
    }
}

// ---------------------------------------------------------------------------
// Agent-binding census + boot liveness readback
// Plan `2026-08-25-agent-class-sessions-reach-coord-like-operator-sessions`,
// Phase 1 — the only phase of that plan that ships.
// ---------------------------------------------------------------------------

/// One AGENT-class binding that [`device_nonce_snapshot`] discarded, as the
/// census records it. The nonce itself is deliberately absent: this is an
/// aggregate forensics line, and a census is not a place to put key material.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentBindingCensusEntry {
    agent_id: Uuid,
    workdir: String,
    /// Always `None` in production TODAY — [`register_agent_proxy_nonce`] binds
    /// `terminal_id: None` by construction, because a headless agent subprocess
    /// never goes through the PTY seam. It is carried anyway because it is the
    /// ONLY key the boot-side liveness join has (see
    /// [`classify_agent_binding_liveness`]), so a future terminal-bearing agent
    /// binding becomes attributable the day it exists — and until then the
    /// census makes the vacuity of that join visible as a column of `null`
    /// rather than hiding it.
    terminal_id: Option<String>,
    minted_at_unix: u64,
}

/// Project the live nonce map down to the AGENT bindings — exactly the set
/// [`device_nonce_snapshot`] drops on the floor. Pure and deterministically
/// ordered, because the emission gate ([`census_should_emit`]) compares
/// consecutive censuses for equality and a nondeterministic order would emit a
/// "change" on every mint.
///
/// Ephemeral bindings cannot appear here: the mint route only ever binds
/// [`ProxyPrincipal::Device`]. The filter is on the principal alone so the
/// census keeps meaning "everything the persist path discarded for being
/// agent-class", which is the question Phase 1 asks.
fn agent_binding_census(map: &HashMap<String, NonceBinding>) -> Vec<AgentBindingCensusEntry> {
    let mut out: Vec<AgentBindingCensusEntry> = map
        .values()
        .filter_map(|b| match b.principal {
            ProxyPrincipal::Agent { agent_id } => Some(AgentBindingCensusEntry {
                agent_id,
                workdir: b.workdir.clone(),
                terminal_id: b.terminal_id.clone(),
                minted_at_unix: minted_at_to_unix(b.minted_at),
            }),
            ProxyPrincipal::Device => None,
        })
        .collect();
    // Total order over every field, so two censuses of the same live set are
    // byte-identical. `agent_id` alone is not total: one agent can hold several
    // bindings (a re-provision into a second worktree).
    out.sort_by(|a, b| {
        a.agent_id
            .cmp(&b.agent_id)
            .then_with(|| a.workdir.cmp(&b.workdir))
            .then_with(|| a.terminal_id.cmp(&b.terminal_id))
            .then_with(|| a.minted_at_unix.cmp(&b.minted_at_unix))
    });
    out
}

/// The last census this process emitted, so [`census_should_emit`] can suppress
/// the unchanged ones. `None` until the first emission — which is why the first
/// census of a process ALWAYS emits, even when it is empty.
static LAST_AGENT_CENSUS: OnceLock<Mutex<Option<Vec<AgentBindingCensusEntry>>>> = OnceLock::new();

fn last_agent_census_cell() -> &'static Mutex<Option<Vec<AgentBindingCensusEntry>>> {
    LAST_AGENT_CENSUS.get_or_init(|| Mutex::new(None))
}

/// Emit-on-change gate, pure over an explicit `previous` slot so it is testable
/// without the process-global.
///
/// **Why change-triggered and not every call.** The census hangs off
/// [`persist_proxy_nonces`], which runs on the mint path — every terminal spawn,
/// every re-provision, debounced only for the *store write*, not for this. A
/// line per call would add thousands of identical rows to the same JSONL the
/// device-side forensics live in, and drown the stream it is meant to sharpen.
/// A line per CHANGE keeps the newest census an accurate statement of the live
/// agent set at the moment that set last moved, which is exactly what the boot
/// readback needs.
///
/// **The first call always emits, empty or not.** The expected steady state of
/// this fleet is zero agent bindings, so "no line" would be the normal case —
/// and then a census that silently stopped running would be indistinguishable
/// from a healthy zero. That confusion is the failure class the whole plan is
/// about (`verification-and-evidence` `silent-empty-is-unknown`), so a boot
/// always states its zero out loud.
fn census_should_emit(
    previous: &mut Option<Vec<AgentBindingCensusEntry>>,
    next: &[AgentBindingCensusEntry],
) -> bool {
    let changed = match previous.as_deref() {
        None => true,
        Some(prev) => prev != next,
    };
    if changed {
        *previous = Some(next.to_vec());
    }
    changed
}

/// Render the census payload into the `extra` fields of a rotation line. Split
/// out so the exact emitted JSON shape is assertable without the filesystem.
fn agent_census_extra(
    entries: &[AgentBindingCensusEntry],
) -> Vec<(&'static str, serde_json::Value)> {
    let bindings: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "agent_id": e.agent_id.to_string(),
                "workdir": e.workdir,
                // `null`, never `"unknown"`: an agent binding has no terminal BY
                // CONSTRUCTION, which is a different fact from a terminal the
                // runner failed to record. The `reject` line already draws that
                // distinction and this must not blur it.
                "terminal_id": e.terminal_id,
                "minted_at_unix": e.minted_at_unix,
            })
        })
        .collect();
    vec![
        ("agent_bindings", serde_json::Value::from(entries.len())),
        ("bindings", serde_json::Value::Array(bindings)),
    ]
}

/// Emit the `agent_binding_census` rotation event for `map`, if the agent set
/// changed since the last emission (or this is the first of the process).
///
/// ## Why this census exists at all
///
/// [`device_nonce_snapshot`] filters `principal == ProxyPrincipal::Device`
/// (OQ3), so every agent binding is discarded on the way to disk. That drop
/// rests on one premise, stated verbatim beside [`AGENT_TOKENS`]: *"AGENT
/// bindings are NEVER persisted (OQ3): a restarted runner has no live agent
/// session, so a restored agent nonce MUST hard-fail closed."*
///
/// **Nobody re-tested that premise for a month.** Phase 1 of plan
/// `2026-08-25-agent-class-sessions-reach-coord-like-operator-sessions` did,
/// and it HOLDS: measured 2026-08-25, of 114 open lifecycle records exactly one
/// predated the 2026-08-24T16:25Z rebuild (a stale `powershell.exe` row last
/// seen 2026-08-19); no open record lived in a workdir that had ever taken an
/// `agent mint`; and the 2592 recorded agent mints span 2592 DISTINCT workdirs
/// — 1.00x, no agent workdir is ever revisited, because every spawn gets a
/// fresh `qontinui-worktrees/<uuid>/<repo>`. Agent sessions are per-task
/// ephemeral: they do not outlive their own task, let alone a restart. Phases
/// 2-6 of that plan (a coord re-mint route, persisted agent bindings, bearer
/// recovery at boot) were closed on that result and deliberately NOT built.
///
/// So this is not recovery machinery. It is the standing detector that makes
/// the premise self-monitoring, because the defect was never a wrong premise —
/// it was an unwatched one. If agent sessions ever start outliving the runner,
/// the pair of events this module emits is what says so: a non-empty
/// `agent_binding_census`, followed at the next boot by an
/// `agent_binding_liveness` line whose `alive` count is non-zero.
///
/// Best-effort and infallible like every other rotation emission, and it must
/// be called WITHOUT the nonce-registry lock held — [`log_rotation_event_with`]
/// does file I/O.
fn note_agent_binding_census(map: &HashMap<String, NonceBinding>) {
    let entries = agent_binding_census(map);
    let emit = match last_agent_census_cell().lock() {
        Ok(mut prev) => census_should_emit(&mut prev, &entries),
        // A poisoned gate must not silence the census — emitting a duplicate
        // line is strictly better than losing the class this code exists to
        // watch.
        Err(_) => true,
    };
    if !emit {
        return;
    }
    log_rotation_event_with(
        "agent_binding_census",
        ROTATION_UNKNOWN,
        ROTATION_UNKNOWN,
        "live AGENT-class nonce bindings discarded by device_nonce_snapshot (never persisted, OQ3)",
        &agent_census_extra(&entries),
    );
}

// --- boot side: is any of what the last census held still running? ----------

/// How much of the tail of the rotation JSONL the boot readback reads. The
/// stream is append-only and unbounded (9k lines on the operator's box at the
/// time of writing, and it only grows), so the readback bounds its own cost
/// instead of loading whatever has accumulated. 2 MiB is ~8k lines at the
/// observed line size — far more than the one census line it is looking for,
/// which is emitted on change and therefore lands near the end.
const ROTATION_LOG_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// The last `agent_binding_census` line, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LastAgentCensus {
    /// The line's own `ts`, verbatim, for the emitted forensics.
    ts: String,
    /// `ts` as epoch seconds. `None` when it did not parse — which downgrades
    /// every verdict to UNKNOWN rather than guessing, because that timestamp is
    /// the PID-reuse guard.
    ts_unix: Option<i64>,
    /// The runner that wrote it.
    runner_id: String,
    /// The pid that died — the root of the survivor probe.
    pid: u32,
    entries: Vec<AgentBindingCensusEntry>,
    /// The count the LINE ITSELF declared (`agent_bindings`), independent of how
    /// many rows decoded. `None` only for a line written without the field.
    ///
    /// This is the integrity check on the decode. Without it, a row whose
    /// `agent_id` fails to parse is dropped by `filter_map` and a boot that
    /// stranded three bindings reports a healthy zero — byte-identical to the
    /// fleet's normal steady state, which is exactly the confusion this whole
    /// module exists to prevent.
    declared_bindings: Option<u64>,
    /// Whether `bindings` was present AND an array. A missing or non-array key
    /// deserializes to an empty Vec through `unwrap_or_default`, which is the
    /// same silent zero by a different route — so the presence of the key is
    /// recorded rather than inferred from the row count.
    rows_present: bool,
}

impl LastAgentCensus {
    /// How many bindings the census SAYS it held: its own declared count, or
    /// the decoded row count when the line carried no count to check against.
    fn declared_len(&self) -> usize {
        self.declared_bindings
            .map(|n| n as usize)
            .unwrap_or(self.entries.len())
    }

    /// True when the rows can be trusted to represent the census: the
    /// `bindings` array was present, and every row it declared decoded.
    ///
    /// False is UNKNOWN, never zero — see [`classify_agent_binding_liveness`],
    /// which refuses to classify rather than reporting the rows that happened
    /// to survive a decode.
    fn rows_decodable(&self) -> bool {
        self.rows_present && self.declared_len() == self.entries.len()
    }
}

/// Find the most recent `agent_binding_census` line in a slice of the rotation
/// JSONL. Pure over the text, so the boot verdict is testable against a literal
/// log fixture.
///
/// Scans backwards and stops at the first hit: censuses supersede one another,
/// and an older one describes a set that has already moved.
///
/// `exclude_pid` drops censuses THIS process wrote. The boot task runs a few
/// seconds in, by which time a restored terminal may already have minted and
/// emitted a census of its own — and reading that one would make the readback
/// compare the live runner against itself, root the survivor probe at a pid
/// that is obviously alive, and report a meaningless `unknown` for everything.
/// The predecessor's census is the only one that can answer the question.
fn parse_last_agent_binding_census(tail: &str, exclude_pid: u32) -> Option<LastAgentCensus> {
    for line in tail.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // a torn or foreign line is skipped, never fatal
        };
        if v["event"] != "agent_binding_census" {
            continue;
        }
        if v["pid"].as_u64() == Some(u64::from(exclude_pid)) {
            continue; // our own line, not the predecessor's
        }
        let ts = v["ts"].as_str().unwrap_or_default().to_string();
        let ts_unix = chrono::DateTime::parse_from_rfc3339(&ts)
            .ok()
            .map(|t| t.timestamp());
        // Both halves of the decode are recorded, not just its output: whether
        // the `bindings` key was an array at all, and how many rows the line
        // said it held. A dropped row and an absent key would otherwise both
        // read as a census of zero.
        let rows = v["bindings"].as_array();
        let rows_present = rows.is_some();
        let entries = rows
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some(AgentBindingCensusEntry {
                            agent_id: Uuid::parse_str(r["agent_id"].as_str()?).ok()?,
                            workdir: r["workdir"].as_str().unwrap_or_default().to_string(),
                            terminal_id: r["terminal_id"].as_str().map(str::to_owned),
                            minted_at_unix: r["minted_at_unix"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Some(LastAgentCensus {
            ts,
            ts_unix,
            runner_id: v["runner_id"]
                .as_str()
                .unwrap_or(ROTATION_UNKNOWN)
                .to_string(),
            pid: v["pid"].as_u64().unwrap_or(0) as u32,
            entries,
            declared_bindings: v["agent_bindings"].as_u64(),
            rows_present,
        });
    }
    None
}

/// What the boot readback concluded about the agent bindings the last census
/// held. `unknown` is a first-class bucket, not a rounding of `dead`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentLivenessTally {
    agent_bindings: usize,
    alive: usize,
    dead: usize,
    unknown: usize,
    /// Live claude-image processes still hanging off the PREVIOUS runner's pid
    /// that were created no later than the census. **This is the (i)-vs-(ii)
    /// discriminator**: zero survivors means the previous runner's whole claude
    /// subtree died with it; a non-zero count means something outlived it.
    survivors: usize,
    /// Which oracle produced the verdicts — so a reader never has to infer it.
    signal: &'static str,
}

impl AgentLivenessTally {
    /// Every binding UNKNOWN, because the oracle could not run. Never `dead`:
    /// an unavailable signal is UNKNOWN, not zero
    /// (`verification-and-evidence` `silent-empty-is-unknown`).
    fn all_unknown(n: usize, signal: &'static str) -> Self {
        Self {
            agent_bindings: n,
            alive: 0,
            dead: 0,
            unknown: n,
            survivors: 0,
            signal,
        }
    }
}

/// Classify each agent binding the last census held against the live process
/// table. Pure over its inputs — a synthetic [`ProcessSnapshot`] exercises every
/// arm without a real process, the same testability posture as
/// [`crate::session::tracking_health::evaluate`], whose primitives this REUSES
/// rather than reimplementing.
///
/// ## The two probes, and what each can actually prove
///
/// 1. **Survivor probe (class-level, a true OS signal).**
///    `claude_pids_in_inclusive_subtree(census.pid, …)` rooted at the PREVIOUS
///    runner's pid. On Windows an orphan keeps its now-dangling
///    `ParentProcessId`, so a claude child that outlived the runner is still
///    reachable from that root. Survivors are filtered to processes created no
///    later than the census, which doubles as the PID-reuse guard: a recycled
///    pid's children are all newer than the census and drop out. This answers
///    *did ANYTHING of the previous runner's claude subtree survive its death* —
///    precisely the question that separates "agent sessions die with the runner"
///    from "they survive and go quiet after the first 401".
/// 2. **Terminal join (per-binding attribution).** `terminal_id` → this boot's
///    live PTY pid → `claude_present_in_inclusive_subtree`, with the runner's
///    OWN boot time as the reuse reference (never a per-record timestamp — see
///    that function's doc comment for the regression that rule came from). This
///    is the only key that can pin a surviving process to a SPECIFIC binding.
///
/// **The terminal join is vacuous today, and the census shows why.**
/// [`register_agent_proxy_nonce`] binds `terminal_id: None` unconditionally, so
/// every production census row carries `null` there and no agent binding can be
/// individually attributed. That is not papered over: a binding that cannot be
/// attributed WHILE survivors exist lands in `unknown`, not in `dead`. `dead` is
/// asserted only when the survivor probe found NOTHING — the one case where the
/// evidence genuinely covers every binding at once.
fn classify_agent_binding_liveness(
    census: &LastAgentCensus,
    snapshot: &crate::process_capture::process_tree::ProcessSnapshot,
    terminal_pids: &HashMap<String, u32>,
    boot_unix_millis: i64,
) -> AgentLivenessTally {
    use crate::process_capture::process_tree::{
        claude_pids_in_inclusive_subtree, claude_present_in_inclusive_subtree,
    };

    let n = census.entries.len();
    // An empty parent map means "could not read the process table", not "no
    // processes" — the same posture as the tracking-health tick and
    // `live_pids_from_snapshot`, both of which refuse to answer rather than
    // answer zero.
    if snapshot.parent_map.is_empty() {
        return AgentLivenessTally::all_unknown(n, "process_table_unavailable");
    }
    // The rows must account for the count the line declared. A partially
    // decoded census would otherwise be classified as if the rows that
    // survived were the whole set, and a fully undecodable one as a zero.
    if !census.rows_decodable() {
        return AgentLivenessTally::all_unknown(census.declared_len(), "census_rows_undecodable");
    }
    let Some(census_unix) = census.ts_unix else {
        return AgentLivenessTally::all_unknown(n, "census_timestamp_unparseable");
    };
    if census.pid == 0 {
        return AgentLivenessTally::all_unknown(n, "census_pid_absent");
    }
    // The census's own pid must be GONE for its subtree to mean "what outlived
    // the restart". Presence in the process table splits two ways, and neither
    // may be classified:
    if let Some(&created) = snapshot.creation_times.get(&census.pid) {
        if created > 0 && created > census_unix {
            // Created after the line it supposedly wrote — a recycled pid whose
            // subtree belongs to a stranger.
            return AgentLivenessTally::all_unknown(n, "prev_runner_pid_recycled");
        }
        // Live and predating its own census: this IS the writer, still running.
        // Two runner processes can share one log dir (an overlapping shutdown,
        // or a second launch of the same instance), and classifying then would
        // report a live peer's every binding as `dead`.
        //
        // No image comparison is needed to reach that conclusion, and refusing
        // without one is strictly safer: a pid can only be recycled after the
        // death that freed it, so a live pid whose creation predates its own
        // census line is the writer by construction. The one residual case —
        // a reuse landing inside the same one-second WMI granularity as the
        // census ts — is also refused here, which is the correct answer for it
        // too.
        return AgentLivenessTally::all_unknown(n, "prev_runner_still_alive");
    }

    // A survivor is a claude process that existed BEFORE THIS BOOT. The
    // reference is the boot instant, NOT the census ts: the census is written
    // at nonce-mint time (`agent_runtime.rs:3900`) and the claude child is
    // spawned afterwards (`:3978`, behind an HTTP probe at `:3905` and, on the
    // respawn arm, minutes or hours of prior run). Creation times are
    // second-granular, so keying on the census ts excluded every real survivor
    // — `survivors` was always empty, every binding fell to `dead`, and the
    // detector could never fire for the live session it was built to catch.
    // The same `PID_REUSE_SKEW_MS` slack `claude_present_in_inclusive_subtree`
    // uses covers the seconds-vs-millis rounding.
    let boot_cutoff_unix = boot_unix_millis
        .saturating_add(crate::process_capture::process_tree::PID_REUSE_SKEW_MS)
        / 1000;
    let survivors: Vec<u32> = claude_pids_in_inclusive_subtree(census.pid, snapshot)
        .into_iter()
        // An unresolvable creation time (`0` — a WMI / `/proc` miss) passes on
        // purpose: counting it as a survivor pushes bindings toward `unknown`
        // instead of toward `dead`, which is the conservative direction.
        .filter(|p| snapshot.creation_times.get(p).copied().unwrap_or(0) <= boot_cutoff_unix)
        .collect();

    let mut tally = AgentLivenessTally {
        agent_bindings: n,
        alive: 0,
        dead: 0,
        unknown: 0,
        survivors: survivors.len(),
        signal: "prev_runner_subtree+terminal_join",
    };
    for e in &census.entries {
        let attributed = e
            .terminal_id
            .as_deref()
            .and_then(|t| terminal_pids.get(t))
            .map(|&pid| claude_present_in_inclusive_subtree(pid, snapshot, boot_unix_millis))
            .unwrap_or(false);
        if attributed {
            tally.alive += 1;
        } else if survivors.is_empty() {
            // Nothing whatsoever outlived the previous runner, so this binding's
            // session did not either. This is the arm that confirms OQ3.
            tally.dead += 1;
        } else {
            // Something outlived the runner, but nothing ties it to THIS binding
            // (there is no terminal to join on). Honest verdict: unknown.
            tally.unknown += 1;
        }
    }
    tally
}

/// Outcome of the boot tail read. Three cases that a bare `Option<String>`
/// collapsed into one silent `return`:
///
/// - [`RotationTail::EmissionOff`] — no log dir resolves, i.e. the test
///   default. Nothing to say and nowhere to say it.
/// - [`RotationTail::Text`] — readable, INCLUDING an absent file, which is a
///   genuine "nothing to read" (a first boot on a fresh install). It flows into
///   the no-census branch, which announces `census_found: false`.
/// - [`RotationTail::Unreadable`] — the file exists but could not be read
///   (permissions, a locked handle, a bad seek). The runner CAN still write, so
///   staying silent here produces a log holding a census and no liveness line —
///   indistinguishable from a pre-instrumentation build, and more alarming than
///   the case that does speak up.
enum RotationTail {
    Text(String),
    EmissionOff,
    Unreadable(String),
}

/// Read the last [`ROTATION_LOG_TAIL_BYTES`] of the rotation JSONL.
fn read_rotation_log_tail() -> RotationTail {
    let Some(dir) = rotation_log_dir() else {
        return RotationTail::EmissionOff;
    };
    read_rotation_log_tail_at(&dir.join(ROTATION_LOG_FILE))
}

/// [`read_rotation_log_tail`] against an explicit path, so the absent-file and
/// unreadable arms are testable without touching the real dev-logs dir.
fn read_rotation_log_tail_at(path: &Path) -> RotationTail {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        // No file yet is not a failure: it is an empty log, and the caller's
        // no-census branch says so out loud.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return RotationTail::Text(String::new())
        }
        Err(e) => return RotationTail::Unreadable(format!("open failed: {e}")),
    };
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(e) => return RotationTail::Unreadable(format!("metadata failed: {e}")),
    };
    let from = len.saturating_sub(ROTATION_LOG_TAIL_BYTES);
    if let Err(e) = f.seek(SeekFrom::Start(from)) {
        return RotationTail::Unreadable(format!("seek failed: {e}"));
    }
    let mut buf = Vec::new();
    if let Err(e) = f.read_to_end(&mut buf) {
        return RotationTail::Unreadable(format!("read failed: {e}"));
    }
    let raw = String::from_utf8_lossy(&buf).into_owned();
    RotationTail::Text(drop_partial_first_line(&raw, from > 0).to_string())
}

/// Drop the first line of a mid-file tail read — it is almost certainly a
/// fragment of a longer line, and a fragment of JSON parses as nothing. A read
/// that started at byte 0 is whole and is returned untouched.
fn drop_partial_first_line(raw: &str, truncated: bool) -> &str {
    if !truncated {
        return raw;
    }
    match raw.find('\n') {
        Some(i) => &raw[i + 1..],
        // No newline at all: the entire tail is one fragment.
        None => "",
    }
}

/// Boot half of Phase 1: read back the last agent-binding census and say, for
/// the bindings it held, whether their sessions are still running.
///
/// Emits exactly one `agent_binding_liveness` rotation line. That line always
/// distinguishes the three states a naive implementation collapses into one:
///
/// - **no census at all** — `census_found: false`, counts `null`. UNKNOWN (the
///   previous runner predates this instrumentation, or the line aged out of the
///   tail), and it must never read as a healthy zero.
/// - **a census of zero** — `census_found: true`, `agent_bindings: 0`. The
///   expected steady state on this fleet, stated explicitly.
/// - **a census with bindings** — `alive` / `dead` / `unknown` over them, plus
///   `survivors`, the class-level discriminator.
///
/// `runner_id` + `pid` on the line are THIS boot's (every rotation line carries
/// them); `prev_runner_id` + `prev_pid` name the process that died. That pair is
/// the restart boundary the whole measurement hangs on.
pub(crate) async fn log_agent_binding_liveness_at_boot(
    terminal_pids: HashMap<String, u32>,
    boot_unix_millis: i64,
) {
    let tail = match read_rotation_log_tail() {
        RotationTail::Text(t) => t,
        // Nowhere to write either — the only arm that may stay silent.
        RotationTail::EmissionOff => return,
        RotationTail::Unreadable(why) => {
            log_rotation_event_with(
                "agent_binding_liveness",
                ROTATION_UNKNOWN,
                ROTATION_UNKNOWN,
                &format!(
                    "rotation log present but unreadable ({why}) — the census could not be read \
                     back. UNKNOWN: this is NOT 'no census', and a silent skip here would look \
                     exactly like a pre-instrumentation build."
                ),
                &[
                    ("census_found", serde_json::Value::Null),
                    ("agent_bindings", serde_json::Value::Null),
                    ("alive", serde_json::Value::Null),
                    ("dead", serde_json::Value::Null),
                    ("unknown", serde_json::Value::Null),
                    ("signal", serde_json::Value::from("rotation_log_unreadable")),
                ],
            );
            return;
        }
    };
    let Some(census) = parse_last_agent_binding_census(&tail, std::process::id()) else {
        log_rotation_event_with(
            "agent_binding_liveness",
            ROTATION_UNKNOWN,
            ROTATION_UNKNOWN,
            "no agent_binding_census line in the rotation log tail — the previous runner \
             predates this instrumentation, or the line aged out. UNKNOWN, not zero.",
            &[
                ("census_found", serde_json::Value::Bool(false)),
                ("agent_bindings", serde_json::Value::Null),
                ("alive", serde_json::Value::Null),
                ("dead", serde_json::Value::Null),
                ("unknown", serde_json::Value::Null),
            ],
        );
        return;
    };

    // Only pay for a process-table snapshot when a census exists AND held
    // something. A zero census needs no oracle — there is nothing to classify,
    // and the snapshot is a PowerShell / `/proc` sweep.
    //
    // `rows_decodable()` gates the shortcut: an undecodable census also decodes
    // to zero rows, and taking this arm for it would emit the healthy-zero line
    // for a boot whose bindings could not be read at all.
    let tally = if census.rows_decodable() && census.entries.is_empty() {
        AgentLivenessTally {
            agent_bindings: 0,
            alive: 0,
            dead: 0,
            unknown: 0,
            survivors: 0,
            signal: "not_probed (census held zero agent bindings)",
        }
    } else if !census.rows_decodable() {
        // Refused without a snapshot: the rows are untrustworthy, so no probe
        // over them could mean anything.
        AgentLivenessTally::all_unknown(census.declared_len(), "census_rows_undecodable")
    } else {
        let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
        classify_agent_binding_liveness(&census, &snapshot, &terminal_pids, boot_unix_millis)
    };

    log_rotation_event_with(
        "agent_binding_liveness",
        ROTATION_UNKNOWN,
        ROTATION_UNKNOWN,
        "boot readback of the last agent-binding census against the live process table",
        &[
            ("census_found", serde_json::Value::Bool(true)),
            ("census_ts", serde_json::Value::from(census.ts.clone())),
            (
                "prev_runner_id",
                serde_json::Value::from(census.runner_id.clone()),
            ),
            ("prev_pid", serde_json::Value::from(census.pid)),
            (
                "agent_bindings",
                serde_json::Value::from(tally.agent_bindings),
            ),
            ("alive", serde_json::Value::from(tally.alive)),
            ("dead", serde_json::Value::from(tally.dead)),
            ("unknown", serde_json::Value::from(tally.unknown)),
            ("survivors", serde_json::Value::from(tally.survivors)),
            ("signal", serde_json::Value::from(tally.signal)),
        ],
    );
}

/// State the live agent-binding set ONCE at boot, whatever it is.
///
/// Without this the census would only ever be emitted off a mint or an agent
/// teardown ([`persist_proxy_nonces`] / [`revoke_agent_proxy_nonces`]) — so a
/// runner that boots and sits idle would emit no census at all, and "the census
/// never ran" would be the steady state on exactly the fleet whose steady state
/// is ZERO agent bindings. Those two must never look alike
/// (`verification-and-evidence` `silent-empty-is-unknown`), and the emit-on-
/// change gate's first-call rule ([`census_should_emit`]) only helps if
/// something calls it. This is that something: one guaranteed line per boot.
///
/// **Call it AFTER [`log_agent_binding_liveness_at_boot`].** The readback wants
/// the PREDECESSOR's census; writing ours first would only add a line it has to
/// skip (it filters on pid, so ordering is a clarity guarantee rather than a
/// correctness one).
pub(crate) fn log_agent_binding_census_at_boot() {
    // Clone under the lock, emit outside it — file I/O must never run with the
    // registry held. Once per boot, so the clone is not on any hot path.
    let snapshot = {
        let map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.clone()
    };
    note_agent_binding_census(&snapshot);
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
    // Census the bindings the snapshot is about to DISCARD, BEFORE the
    // persistence gate. The census is forensics, not persistence: turning
    // `COORD_MCP_PERSIST_NONCES=0` off should not blind the detector that
    // watches OQ3's premise (see [`note_agent_binding_census`]). Emitted from
    // here rather than from inside [`device_nonce_snapshot`] so that function
    // stays pure — this is the production chokepoint every mint already flows
    // through, and every caller passes an OWNED snapshot, so no registry lock
    // is held across the file I/O.
    note_agent_binding_census(map);
    if !nonce_persistence_enabled() {
        return;
    }
    enqueue_nonce_persist(device_nonce_snapshot(map));
}

/// Debounce window for the encrypted whole-store nonce write.
///
/// Persisting a nonce costs a full decrypt + parse of the ENTIRE `StoredTokens`
/// document (`load_tokens_for_write`) and then a re-serialize + AES-GCM encrypt
/// + atomic rewrite of the whole thing (`save_tokens`) — for a single map entry.
/// That sat on the terminal spawn path, once per spawn. Coalescing means a burst
/// of spawns (a boot restore of 40 panes) pays ONE store rewrite instead of 40.
const NONCE_PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(750);

/// Coalescing state for the debounced nonce persist.
#[derive(Default)]
struct NoncePersistQueue {
    /// The newest snapshot awaiting a write, if any.
    pending: Option<HashMap<String, crate::secure_storage::StoredNonceBinding>>,
    /// Whether the flush thread is alive (it drains until `pending` is empty).
    flushing: bool,
    /// The last snapshot actually written, so an unchanged map costs nothing.
    last_written: Option<HashMap<String, crate::secure_storage::StoredNonceBinding>>,
    /// Consecutive failed write attempts for the CURRENT pending snapshot.
    /// Bounds the failure re-queue (see [`flush_nonce_persist_once`]) so a
    /// permanently broken store retries a few times instead of spinning the
    /// flush thread — and its warn — every debounce window forever. Reset by a
    /// successful write and by a NEWER snapshot arriving.
    failed_attempts: u32,
}

/// How many times a failed nonce write is re-queued before the snapshot is
/// abandoned. Losing it is the store's designed failure mode (an unrestored
/// nonce 401s and the next provisioning re-mints), so a few retries buy back
/// the transient case without turning a permanent failure into a busy loop.
const NONCE_PERSIST_MAX_ATTEMPTS: u32 = 3;

static NONCE_PERSIST: once_cell::sync::Lazy<std::sync::Mutex<NoncePersistQueue>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(NoncePersistQueue::default()));

/// Queue `snapshot` for a debounced write, starting the flush thread if needed.
///
/// Losing an in-flight snapshot to a crash is already the designed failure mode
/// of this store: an unrestored nonce simply 401s and the next provisioning
/// re-mints (see [`crate::secure_storage::SecureStorage::load_coord_mcp_nonces`]).
/// The in-memory registry stays authoritative for this process either way, so
/// nothing a live session depends on rides the debounce.
fn enqueue_nonce_persist(snapshot: HashMap<String, crate::secure_storage::StoredNonceBinding>) {
    let start_thread = {
        let mut q = match NONCE_PERSIST.lock() {
            Ok(q) => q,
            Err(e) => {
                warn!("coord_mcp: nonce persist queue poisoned ({e}) — nonces not persisted");
                return;
            }
        };
        if q.last_written.as_ref() == Some(&snapshot) && q.pending.is_none() {
            return; // already durable and nothing newer queued
        }
        q.pending = Some(snapshot);
        // A newer snapshot supersedes whatever was failing — give it a full
        // retry budget of its own.
        q.failed_attempts = 0;
        if q.flushing {
            false
        } else {
            q.flushing = true;
            true
        }
    };
    if !start_thread {
        return; // an existing flush thread will pick the newer snapshot up
    }
    let spawned = std::thread::Builder::new()
        .name("coord-mcp-nonce-persist".to_string())
        .spawn(flush_nonce_persist_loop);
    if let Err(e) = spawned {
        warn!("coord_mcp: could not start nonce persist thread ({e}) — persisting inline");
        if let Ok(mut q) = NONCE_PERSIST.lock() {
            q.flushing = false;
        }
        flush_nonce_persist_once();
    }
}

/// Sleep out the debounce, write, and repeat while newer snapshots keep landing.
fn flush_nonce_persist_loop() {
    loop {
        std::thread::sleep(NONCE_PERSIST_DEBOUNCE);
        flush_nonce_persist_once();
        match NONCE_PERSIST.lock() {
            Ok(mut q) => {
                if q.pending.is_none() {
                    q.flushing = false;
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Write whatever snapshot is pending (if any) to the encrypted store.
///
/// On a write failure the snapshot is put BACK on the queue rather than
/// dropped: `pending` was `take`n and `last_written` was not updated, so
/// without the re-queue the only thing that could ever retry it is another
/// nonce registration happening to land — a transient store error would
/// otherwise silently lose the newest bindings until the next spawn.
fn flush_nonce_persist_once() {
    let snapshot = match NONCE_PERSIST.lock() {
        Ok(mut q) => match q.pending.take() {
            Some(s) => s,
            None => return,
        },
        Err(_) => return,
    };
    // Re-queue `snapshot` for another attempt, but never over a NEWER one a
    // concurrent `enqueue_nonce_persist` has already parked, and only while the
    // retry budget lasts.
    fn requeue(snapshot: HashMap<String, crate::secure_storage::StoredNonceBinding>) {
        let Ok(mut q) = NONCE_PERSIST.lock() else {
            return;
        };
        if q.pending.is_some() {
            return; // a newer snapshot already supersedes this one
        }
        q.failed_attempts = q.failed_attempts.saturating_add(1);
        if q.failed_attempts >= NONCE_PERSIST_MAX_ATTEMPTS {
            warn!(
                attempts = q.failed_attempts,
                "coord_mcp: giving up persisting proxy nonces — the next provisioning re-mints"
            );
            return;
        }
        q.pending = Some(snapshot);
    }
    match crate::secure_storage::SecureStorage::new() {
        Ok(store) => {
            if let Err(e) = store.store_coord_mcp_nonces(&snapshot) {
                warn!("coord_mcp: failed to persist proxy nonces: {e}");
                requeue(snapshot);
                return;
            }
            if let Ok(mut q) = NONCE_PERSIST.lock() {
                q.last_written = Some(snapshot);
                q.failed_attempts = 0;
            }
        }
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, proxy nonces not persisted: {e}");
            requeue(snapshot);
        }
    }
}

/// Mirror `map` into the GIVEN store. The store is injected so the persistence
/// path is unit-testable against a temp-dir [`SecureStorage::with_path`] without
/// mutating `QONTINUI_SECURE_STORAGE_DIR` (which is process-global and pollutes
/// every other test that reads the default store). The `nonce_persistence_enabled`
/// gate is the CALLER's concern — handing a store IS the decision to persist.
/// Test-only since the production path became debounced ([`enqueue_nonce_persist`]):
/// the default-store write now happens on the flush thread, which owns its own
/// `SecureStorage`. Kept because it is the seam the persistence tests use to
/// assert WHAT gets persisted without touching the developer's real store.
#[cfg(test)]
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

/// What a boot restore actually did — the two numbers the boot summary needs,
/// kept apart because conflating them is what defeated the Change-2 smell test
/// (plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs`, Phase 3).
///
/// The 2026-08-24 incident is the worked example: the store held exactly one
/// binding, that binding was already live, so `inserted` was **0** — the restore
/// recovered nothing — while the live map size was **1**. The boot line printed
/// the live map size under the word `restored`, which read as a healthy boot and
/// meant the "restored 0 then root had to Rewrite" warning could never fire,
/// because the printed number was never the recovered count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct NonceRestoreOutcome {
    /// Bindings the restore GENUINELY recovered — persisted entries that were
    /// not already present in the live map. This is the honest "did the restore
    /// work" number, and the one the boot summary prints as `restored`.
    pub(crate) inserted: usize,
    /// Size of the live nonce registry AFTER the merge. A third number: it
    /// counts this process's own mints as well as anything restored, so it can
    /// be non-zero on a restore that recovered nothing. Reported under its own
    /// name so it stays available without ever standing in for `inserted`.
    pub(crate) live_map_len: usize,
}

/// Restore persisted proxy nonces into the in-memory registry on boot (Phase
/// 3b). Idempotent + run-once: merges the persisted set UNDER any nonces already
/// minted this process (live mints win on key collision, which cannot happen in
/// practice — the persisted set predates this process). No-op when persistence
/// is disabled. Wire this into the same startup path as the other auto-start
/// tasks so already-written `.mcp.json` nonces keep validating post-restart.
///
/// Returns a [`NonceRestoreOutcome`] — `inserted` (what was actually recovered)
/// AND `live_map_len` (the registry size afterwards). Both are `0` when
/// persistence is disabled (on every call, not just the first), when storage is
/// unavailable, or when nothing was persisted. `inserted` is what the boot task
/// surfaces as `restored` (plan 2026-07-07 Change 2 observability, corrected by
/// plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 3), so
/// a silent rotation — restore brought back 0 then self-heal had to mint fresh —
/// is visible in the logs.
///
/// "Run-once" is scoped to the arms that actually restore: a
/// persistence-disabled call is a no-op that leaves the restore still
/// available, so flipping `COORD_MCP_PERSIST_NONCES` on mid-process and calling
/// again works. Each outcome still logs exactly one aggregate `restore` line
/// per process.
pub(crate) fn restore_proxy_nonces_from_store() -> NonceRestoreOutcome {
    // The persistence-disabled arm gets its OWN one-shot rather than consuming
    // the restore guard. Both properties matter and they are not the same
    // property:
    //
    //   * exactly ONE aggregate `restore` line per process per outcome — an
    //     aggregate line cannot be attributed to a caller (see
    //     `log_restore_event`), so repeating it on every call poisons the
    //     stream. That is what the one-shots buy.
    //   * a disabled call must not BURN the restore. Consuming
    //     `PROXY_NONCES_RESTORED` here would mean a process that flips
    //     `COORD_MCP_PERSIST_NONCES` on after a disabled call can never
    //     restore at all, and would also make a second disabled call return
    //     the live map size — contradicting this function's own "returns 0 when
    //     persistence is disabled" contract. No caller does either today; a
    //     guard that is wrong only for callers that do not exist yet is still
    //     wrong, and costs one `OnceLock` to make right.
    if !nonce_persistence_enabled() {
        if PROXY_NONCES_RESTORE_DISABLED_LOGGED.set(()).is_ok() {
            log_restore_event(0, 0, "persistence disabled (COORD_MCP_PERSIST_NONCES=0)");
        }
        return NonceRestoreOutcome::default();
    }
    if PROXY_NONCES_RESTORED.set(()).is_err() {
        // Already restored once this process — this call recovered nothing
        // (`inserted = 0`, which is the literal truth for it), but still reports
        // the current live size so a duplicate boot-task run logs a coherent
        // registry count.
        return NonceRestoreOutcome {
            inserted: 0,
            live_map_len: proxy_nonces()
                .lock()
                .expect("proxy nonce map poisoned")
                .len(),
        };
    }
    let store = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s,
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, cannot restore proxy nonces: {e}");
            log_restore_event(0, 0, "secure storage unavailable");
            return NonceRestoreOutcome::default();
        }
    };
    restore_proxy_nonces_from(&store)
}

/// Emit the `restore` rotation event.
///
/// There was no restore event AT ALL before Phase 3 (the whole 5,490-line
/// production log carried one `adopt` line and zero restores), so "did the boot
/// restore run, and what did it recover?" was unanswerable from disk — the one
/// question the 2026-08-19 reconstruction most needed. It is emitted on EVERY
/// arm, the empty and disabled ones included: a `restore` line reading
/// `restored: 0` is the loud signal that the persisted set was dropped, which
/// is exactly what a deserialization regression in the store schema would look
/// like. Silence would be indistinguishable from a healthy boot.
///
/// Aggregate, so it carries no key material and no workdir — the per-nonce
/// detail is the `mint`/`write` trail those nonces already left before the
/// restart. The workdir field therefore carries [`ROTATION_UNKNOWN`], never the
/// empty string: `"workdir":""` is the exact ambiguity Phase 3 introduced the
/// sentinel to kill (671 production reject lines carried it, and none of them
/// could be attributed), and an aggregate line is a *statement* that there is
/// no single workdir — not a failure to record one.
fn log_restore_event(restored: usize, skipped: usize, reason: &str) {
    log_rotation_event_with(
        "restore",
        ROTATION_UNKNOWN,
        ROTATION_UNKNOWN,
        reason,
        &[
            ("restored", serde_json::Value::from(restored)),
            ("skipped", serde_json::Value::from(skipped)),
        ],
    );
}

/// Merge the persisted nonce set from the GIVEN store into the live in-memory
/// registry (live mints win on collision). The store is injected so the
/// restore path is unit-testable against a temp-dir store without the
/// run-once `PROXY_NONCES_RESTORED` guard or any global-env mutation. Returns
/// both honest counts — see [`NonceRestoreOutcome`].
///
/// Emits one aggregate `restore` rotation event on every path
/// ([`log_restore_event`]) — including the empty-store one, which is the
/// signal a store-schema regression would show up as.
fn restore_proxy_nonces_from(store: &crate::secure_storage::SecureStorage) -> NonceRestoreOutcome {
    let persisted = store.load_coord_mcp_nonces();
    if persisted.is_empty() {
        log_restore_event(
            0,
            0,
            "persisted nonce set empty (nothing to restore, or the store failed to deserialize)",
        );
        return NonceRestoreOutcome {
            inserted: 0,
            live_map_len: proxy_nonces()
                .lock()
                .expect("proxy nonce map poisoned")
                .len(),
        };
    }
    let persisted_total = persisted.len();
    let mut inserted = 0usize;
    // Sampled ONCE for the whole restore (a filesystem read), and held across
    // the registry lock below rather than taken under it.
    let restore_time_pin = crate::session::tenant_pin::resolve_tenant_pin();
    let live_map_len = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        for (nonce, binding) in persisted {
            let vacant = !map.contains_key(&nonce);
            // Only DEVICE bindings are ever persisted (OQ3), so a restored entry
            // is unconditionally a Device principal. An agent nonce can never be
            // restored — its slot is process-global and gone after a restart, so
            // it would hard-fail closed anyway.
            map.entry(nonce).or_insert(NonceBinding {
                // Phase 3c: the persisted store can carry an empty workdir from
                // any runner that predates the normalization, so the restore is
                // a second entry point and needs it too — otherwise the `""`
                // sentinel simply survives a restart.
                workdir: normalize_binding_workdir(&binding.workdir),
                principal: ProxyPrincipal::Device,
                // Only Persistent bindings are ever written to the store
                // (`device_nonce_snapshot`), so a restored entry is
                // unconditionally Persistent — the restore cannot resurrect an
                // ephemeral mint-route nonce as an unbounded one.
                lifetime: NonceLifetime::Persistent,
                // PROVENANCE TELEMETRY, not a credential selector — plan
                // `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
                // Phase 1d. This used to be a hardcoded `Unpinned`, and while
                // `Unpinned` STILL meant "the legacy `access_token` slot" that
                // hardcode silently re-pointed every restored session at a
                // different credential than the one it was provisioned
                // against, at every runner restart. Phase 1b took that
                // authority away: `session_tenant_or_refuse` resolves a
                // tenant-less binding at REQUEST time, so this field is free to
                // say what was actually observed instead of what happened to
                // select the right slot.
                //
                // What was observed is the MACHINE's pin at restore time. It is
                // not a claim about the session — the persisted store carries
                // only (nonce, workdir, terminal), never a tenant, and the
                // original session's tenant died with the previous runner
                // process. It records the one tenant fact that was true when
                // the binding came back.
                session_pin: restore_time_pin,
                // The terminal IS carried, since plan 2026-08-20 Phase 4
                // widened the store to hold it. It is not an identity claim —
                // the PTY it names died with the previous runner process, so
                // caller self-identification still misses on a restored nonce
                // (`terminal_record_missing`, which is the honest verdict).
                // What it restores is the EVICTION KEY. The predicate in
                // `mint_and_register_nonce` is one slot per
                // `(workdir, terminal_id, Persistent)`; reconstructing every
                // restored binding as `None` collapsed all of a shared
                // workdir's restored nonces into ONE slot, so the first
                // persistent mint into that workdir evicted the lot — the
                // measured 33-deep cascade in five seconds against
                // `D:\qontinui-root` on 2026-08-19. `None` is still what a
                // pre-Phase-4 store and a genuinely terminal-less binding
                // restore as.
                terminal_id: binding.terminal_id,
                // The TRUE mint time, from the store. Stamping the restore
                // instant here (what this did before) made every restored
                // binding tie, so the persisted-set cap's oldest-first cut fell
                // entirely to the nonce-string tiebreak the moment the restored
                // pool alone exceeded the cap — a random pick that could drop a
                // live session's credential and keep a dead terminal's. An
                // entry with no persisted age (pre-widening store) lands on
                // `UNIX_EPOCH` and sorts OLDEST. See
                // [`minted_at_from_unix`] and [`NonceBinding::minted_at`].
                minted_at: minted_at_from_unix(binding.minted_at_unix),
            });
            if vacant {
                inserted += 1;
            }
        }
        map.len()
    };
    // Honest counts: `inserted` is what the restore actually recovered,
    // `skipped` is the persisted entries a live mint already occupied (the
    // `or_insert` no-op), and `live_map_len` is the map size afterwards — a
    // THIRD number that counts this process's own mints too.
    //
    // All three used to collapse into one on the way out: the return value was
    // `live_map_len` and the boot task printed it under the word `restored`. On
    // 2026-08-24 that printed `restored 1` for a boot whose `inserted` was 0,
    // which read as healthy and disarmed the Change-2 smell test. The two are
    // now returned SEPARATELY ([`NonceRestoreOutcome`]) so the summary can name
    // each for what it is.
    let skipped = persisted_total.saturating_sub(inserted);
    log_restore_event(inserted, skipped, "boot restore from encrypted store");
    info!(
        "coord_mcp: restored {inserted} persisted proxy nonce(s) from secure storage \
         ({skipped} skipped as already-live; live map now {live_map_len})"
    );
    NonceRestoreOutcome {
        inserted,
        live_map_len,
    }
}

/// Mint + register a fresh PERSISTENT per-session DEVICE proxy nonce for
/// `workdir`, returning it. Any prior persistent nonce registered for the same
/// workdir AND the same `terminal_id` is evicted — a re-provision rewrites the
/// config, so the old nonce is unreachable and keeping it would only widen the
/// accept set. The updated set is mirrored to the encrypted store (Phase 3b) so
/// it survives a restart.
///
/// `terminal_id` is the runner terminal this nonce is being provisioned for, and
/// it is what makes caller self-identification deterministic (see
/// [`NonceBinding::terminal_id`]). It also NARROWS eviction, so two terminals
/// sharing one cwd each keep their own live nonce instead of 401ing each other —
/// see [`mint_and_register_nonce`]'s eviction rule. `None` = a caller with no
/// terminal (the in-cwd `.mcp.json` writer, the boot self-heal), which preserves
/// the previous same-workdir eviction behavior exactly.
///
/// This is the RUNNER-SPAWN path (the identity seam, the terminal chokepoint,
/// the boot self-heal). Its semantics are otherwise deliberately unchanged by
/// plan 2026-07-17 — see [`register_session_proxy_nonce`] for the mint-route path.
fn register_proxy_nonce(workdir: &str, terminal_id: Option<&str>) -> String {
    let (nonce, snapshot) = mint_and_register_nonce(
        workdir,
        ProxyPrincipal::Device,
        NonceLifetime::Persistent,
        terminal_id,
    );
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
///   [`session_identity_marker_present`] — the operator's real kill switch that
///   invalidates ALL ephemeral sessions at once.
///
/// Precise per-nonce revoke is INTENTIONALLY deferred to the credential-exposure
/// plan (`2026-07-17-coord-device-credential-exposure-and-authz-gaps`). Do NOT
/// shorten the TTL to paper over this — it would 401 live sessions mid-turn
/// (the MCP client never re-reads its config) — and do NOT re-add cwd-scoped
/// eviction, which caused the sibling-DoS.
///
/// # No terminal id — by construction
///
/// The binding is minted with `terminal_id: None`. This route exists precisely
/// FOR sessions the runner did not spawn: there is no PTY, so there is no
/// terminal to name, and caller self-identification falls back to the workdir
/// leg for these nonces. Do not invent one here — a fabricated terminal id would
/// resolve confidently to somebody else's session.
fn register_session_proxy_nonce(workdir: &str) -> String {
    let (nonce, _snapshot) = mint_and_register_nonce(
        workdir,
        ProxyPrincipal::Device,
        NonceLifetime::ephemeral(),
        None,
    );
    nonce
}

/// Mint + register a fresh per-session proxy nonce bound to a specific AGENT for
/// `workdir`. Unlike [`register_proxy_nonce`] this is NOT persisted (OQ3) — an
/// agent nonce must hard-fail closed across a restart, which is automatic since
/// [`persist_proxy_nonces`] drops non-device bindings. The per-request bearer
/// comes from the agent's own [`AGENT_TOKENS`] slot, never the device JWT.
///
/// `terminal_id: None` — a headless agent subprocess is spawned directly
/// (`agent_runtime::run_agent_subprocess`), never through the PTY/terminal seam,
/// so it has no terminal to bind. Its identity is the agent JWT, not a caller
/// session header.
pub(crate) fn register_agent_proxy_nonce(workdir: &str, agent_id: Uuid) -> String {
    // Persistent lifetime = today's semantics (no expiry). It is NOT a disk
    // persistence decision: `device_nonce_snapshot` drops every agent binding
    // regardless, so an agent nonce still hard-fails closed across a restart.
    let (nonce, snapshot) = mint_and_register_nonce(
        workdir,
        ProxyPrincipal::Agent { agent_id },
        NonceLifetime::Persistent,
        None,
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
/// **Eviction rule (plan 2026-07-17 §1/E; narrowed by plan
/// 2026-08-04-runner-caller-session-self-id-resolution Stage 1):**
/// - A **PERSISTENT** mint evicts the prior persistent nonces for the same
///   workdir **AND the same `terminal_id`** (the runner-spawn re-provision
///   case) and graces the evicted DEVICE ones.
///
///   The `terminal_id` half of that key is what lets two terminals share one
///   cwd. Evicting on workdir alone was correct while the nonce was a
///   per-workdir credential; now that the identity seam mints one per TERMINAL
///   (see [`NonceBinding::terminal_id`] and [`mcp_config_file_name`]),
///   workdir-only eviction would mean terminal 2's spawn 401s terminal 1's
///   already-connected MCP client — the sibling-DoS the ephemeral class already
///   had to fix. Two live persistent nonces for one workdir is therefore a
///   sanctioned state, exactly as two ephemeral ones already were.
///
///   When BOTH sides carry `terminal_id: None` the rule degenerates to the
///   previous "same workdir + same class" one, byte-for-byte: a re-provision
///   into the same cwd by a terminal-less caller (the in-cwd `.mcp.json`
///   writer, the boot self-heal) still evicts its predecessor. That is why
///   the session-provision chokepoint no longer re-mints a cwd whose file
///   already holds a live nonce ([`reusable_in_cwd_device_nonce`], plan
///   2026-09-02-coord-access-dies-by-eviction-not-expiry Phase F4): in a
///   shared canonical checkout the "predecessor" is a still-live sibling's
///   key, and the `None == None` match made the rule workdir-only in practice.
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
    terminal_id: Option<&str>,
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
    // Typed at the mint (plan Phase 3): the ONE place that reads this machine's
    // own pin, so the Pinned / Unpinned / Unresolvable distinction is captured
    // while it still exists. Every other construction site of `NonceBinding`
    // has no machine pin to read and says `Unpinned` explicitly.
    let session_pin = crate::session::tenant_pin::resolve_tenant_pin();
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
        // Each evicted entry carries the terminal its binding named (plan
        // 2026-09-02-coord-access-dies-by-eviction-not-expiry Phase F4 §3):
        // the rotation log's `evict` line must say WHICH slot was superseded,
        // or the `(workdir, terminal_id)` grouping the eviction rule is keyed
        // on is unreadable from the log — which is exactly what the F4
        // measurement ran into.
        let mut evicted_graceable: Vec<(String, Option<String>)> = Vec::new();
        let mut evicted_agent: Vec<(String, Option<String>)> = Vec::new();
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
            // (2) Class- AND terminal-scoped eviction. Only a PERSISTENT mint
            // evicts, and only the prior PERSISTENT nonces for the same workdir
            // AND the same terminal (the PTY re-provision case — never an
            // ephemeral, so the class-scoping holds). Adding the terminal to the
            // key is what lets two terminals share a cwd without the second
            // spawn 401ing the first one's live MCP client; with both sides
            // `None` it is byte-for-byte the previous same-workdir rule. An
            // EPHEMERAL mint evicts NOTHING: two DIFFERENT bare sessions routinely
            // share a cwd, and an ephemeral eviction is not graced, so removing a
            // sibling ephemeral nonce would 401 the other session's
            // already-connected MCP client mid-session. The DEVICE nonces among
            // the evicted set are collected to ride the device-evicted grace
            // TTL (Change 3; widened by plan 2026-07-27 Phase 5/R3) — an
            // in-flight client that cached one keeps validating until it
            // reconnects; agent nonces are NOT graced (they hard-fail closed on
            // re-mint), so they are dropped without being collected.
            if !ephemeral
                && b.workdir == workdir
                && b.terminal_id.as_deref() == terminal_id
                && !b.lifetime.is_ephemeral()
            {
                if b.principal == ProxyPrincipal::Device {
                    evicted_graceable.push((n.clone(), b.terminal_id.clone()));
                } else {
                    evicted_agent.push((n.clone(), b.terminal_id.clone()));
                }
                return false;
            }
            true
        });
        map.insert(
            nonce.clone(),
            NonceBinding {
                // Phase 3c: normalized at the MINT so `""` never enters the map
                // and every downstream reader — rotation rows, the census, the
                // reject attribution — sees one sentinel instead of two.
                workdir: normalize_binding_workdir(workdir),
                principal,
                lifetime,
                session_pin,
                // Frozen at mint time, exactly like `session_pin`. This is
                // the deterministic leg of caller self-identification — see
                // [`NonceBinding::terminal_id`] / [`terminal_id_for_nonce`].
                terminal_id: terminal_id.map(str::to_string),
                minted_at: std::time::SystemTime::now(),
            },
        );
        let graceable_names: Vec<String> =
            evicted_graceable.iter().map(|(n, _)| n.clone()).collect();
        grace_evicted_device_nonces(&graceable_names);
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
    // Every `mint` and `evict` line names its slot's terminal (F4 §3). `null`
    // is the honest value for a terminal-less binding (in-cwd `.mcp.json`,
    // mint route, adopt, agent) — it is the eviction key's real value there,
    // not a recording failure, and the `reject` line draws the same
    // distinction.
    let grace_cause = rotation_grace_cause();
    for (n, evicted_terminal) in &evicted_device {
        log_rotation_event_with(
            "evict",
            workdir,
            n,
            "superseded by same-workdir+same-terminal persistent re-mint",
            &[(
                "terminal_id",
                serde_json::Value::from(evicted_terminal.clone()),
            )],
        );
        log_rotation_event("grace", workdir, n, &grace_cause);
    }
    for (n, evicted_terminal) in &evicted_agent {
        log_rotation_event_with(
            "evict",
            workdir,
            n,
            "superseded by same-workdir+same-terminal persistent re-mint (agent — fails closed, never graced)",
            &[("terminal_id", serde_json::Value::from(evicted_terminal.clone()))],
        );
    }
    log_rotation_event_with(
        "mint",
        workdir,
        &nonce,
        mint_cause,
        &[("terminal_id", serde_json::Value::from(terminal_id))],
    );
    (nonce, snapshot)
}

/// Evict every proxy nonce bound to `workdir` and persist the shrunken set.
/// Close-time cleanup for PER-SESSION workdirs (relay chat): unlike the stable
/// per-agent dirs, a per-session workdir is never reused, so the same-workdir
/// eviction inside [`mint_and_register_nonce`] never fires for it — without
/// this call its device nonce would stay valid (and persisted) forever.
/// Evicted PERSISTENT device nonces ride the same grace TTL as a re-mint so an
/// in-flight client fails closed only after the window; agent nonces drop
/// immediately, and so do EPHEMERAL device nonces (plan 2026-07-27 Phase 5/R3
/// hardening): grace checks only expiry, never the session-identity opt-out
/// gate that [`live_binding`] enforces on ephemeral bindings, so gracing an
/// ephemeral nonce would let it outlive the operator's kill switch for the
/// whole grace window. Ephemeral nonces stay TTL-bounded on their own class
/// rules instead — the same "grace is for runner-initiated re-provisions of
/// the persistent class only" invariant `mint_and_register_nonce` documents.
pub(crate) fn evict_proxy_nonces_for_workdir(workdir: &str) {
    let (snapshot, evicted_device, evicted_ephemeral, evicted_agent) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        let evicted_device: Vec<String> = map
            .iter()
            .filter(|(_, b)| {
                b.workdir == workdir
                    && b.principal == ProxyPrincipal::Device
                    && !b.lifetime.is_ephemeral()
            })
            .map(|(n, _)| n.clone())
            .collect();
        if evicted_device.is_empty() && !map.values().any(|b| b.workdir == workdir) {
            return; // nothing bound to this workdir — skip the persist write
        }
        let evicted_ephemeral: Vec<String> = map
            .iter()
            .filter(|(_, b)| {
                b.workdir == workdir
                    && b.principal == ProxyPrincipal::Device
                    && b.lifetime.is_ephemeral()
            })
            .map(|(n, _)| n.clone())
            .collect();
        let evicted_agent: Vec<String> = map
            .iter()
            .filter(|(_, b)| b.workdir == workdir && b.principal != ProxyPrincipal::Device)
            .map(|(n, _)| n.clone())
            .collect();
        map.retain(|_, b| b.workdir != workdir);
        grace_evicted_device_nonces(&evicted_device);
        (
            map.clone(),
            evicted_device,
            evicted_ephemeral,
            evicted_agent,
        )
    };
    // Rotation forensics — outside the lock (see `mint_and_register_nonce`).
    let grace_cause = rotation_grace_cause();
    for n in &evicted_device {
        log_rotation_event("evict", workdir, n, "per-session workdir closed");
        log_rotation_event("grace", workdir, n, &grace_cause);
    }
    for n in &evicted_ephemeral {
        log_rotation_event(
            "evict",
            workdir,
            n,
            "per-session workdir closed (ephemeral — never graced, kill switch stays enforceable)",
        );
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

/// The runner TERMINAL a registered proxy nonce was provisioned for
/// ([`NonceBinding::terminal_id`]). `None` for an empty, unregistered, or
/// no-longer-valid nonce ([`live_binding`]) — and also for a live binding that
/// legitimately has no terminal (restored, adopted, mint-route, in-cwd
/// `.mcp.json`, agent).
///
/// This is the **deterministic** leg of session-fabric Phase 0 caller
/// self-identification: `nonce → terminal_id → the OPEN lifecycle record with
/// that terminal → its `claude_session_id` → coord `agent_session_id`. Every
/// hop is 1:1, so there is no `last_seen_at` tie-break and no ambiguity. The
/// [`workdir_for_nonce`] leg stays as the FALLBACK for the terminal-less
/// bindings above, where it is inherently 1:N (a workdir can host many
/// sessions) and therefore a guess.
///
/// Goes through the same [`live_binding`] chokepoint as every other lookup, so
/// expiry and revocation apply identically — an expired ephemeral or an
/// opted-out machine yields `None` here too, never a stale identity.
pub(crate) fn terminal_id_for_nonce(nonce: &str) -> Option<String> {
    live_binding(nonce).and_then(|b| b.terminal_id)
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
            session_identity_marker_present().then_some(binding)
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

/// The typed pin a DEVICE proxy nonce was provisioned under.
///
/// A nonce with no live binding (graced, or already reaped) reads
/// [`TenantPin::Unpinned`], never `Unresolvable`: a graced session is a
/// legitimate one whose binding simply aged out, and the nonce/scope gate in
/// [`proxy_request_gate`] is what decides whether it may proceed at all. Only a
/// binding minted on a machine that could not state its tenant reads
/// `Unresolvable`.
pub(crate) fn proxy_session_pin_for_nonce(nonce: &str) -> crate::session::tenant_pin::TenantPin {
    live_binding(nonce)
        .map(|b| b.session_pin)
        .unwrap_or(crate::session::tenant_pin::TenantPin::Unpinned)
}

/// Typed refusal for a session whose tenant cannot be resolved by ANY route.
///
/// Shaped like [`device_jwt_refreshing_error`] — a structured, diagnosable
/// status rather than a bare 401 — but deliberately **not** retryable: the
/// refreshing error means "come back in a moment", while this means "this
/// machine cannot say who it is, and waiting will not change that". A bare 401
/// here would read as a dead transport and send the operator down the
/// `/coord-revive` path for a configuration problem.
pub(crate) fn tenant_unresolvable_error() -> (u16, String) {
    (
        503,
        "COORD_MCP_PROXY_TENANT_UNRESOLVABLE: this machine cannot resolve its tenant \
         — ~/.qontinui/machine.json is missing or malformed AND the device JWT \
         carries no tenant_id claim. Coord memory and other tenant-scoped writes \
         are refused rather than attributed to the default tenant. Fix by \
         repairing machine.json or re-pairing the device."
            .to_string(),
    )
}

/// Resolve the tenant to select a DEVICE bearer for — or refuse.
///
/// Plan: `2026-08-05-runner-memory-injection-and-tenant-fail-closed` Phase 3,
/// **reworked by `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
/// Phase 1b.** This is the single implementation of the fail-closed decision.
/// All four `mcp_api` bearer-selection sites (the coord-mcp proxy handler, the
/// claims read proxy, the write proxy, and `vcs_create_pull_request`) reach it
/// through [`session_bearer_and_tenant_or_refuse`] instead of re-deriving the
/// rule, because four hand-copied copies of a policy is how three of them
/// silently drift.
///
/// ## What Phase 1b changed, and why
///
/// The binding's [`crate::session::tenant_pin::TenantPin`] is frozen at MINT
/// time. It used to be the sole authority over credential selection, and two
/// of the three mint paths do not know the tenant at all:
/// [`restore_proxy_nonces_from`] and [`adopt_on_disk_nonce`] hardcoded
/// `Unpinned`, which mapped to "the legacy `access_token` slot" — so after
/// every runner restart every restored session silently swapped to a different
/// credential slot than the one it was provisioned against. **Provenance is
/// not a credential.**
///
/// The pin is now PROVENANCE TELEMETRY. It keeps exactly one authority, and
/// only because nothing else can supply it: when it names a tenant explicitly,
/// that tenant is the SESSION's, and `machine.json` — which records a single
/// *active* tenant — cannot distinguish two co-resident ones. Everything else
/// is resolved **at request time**.
///
/// ## The rule (authority order)
///
/// | # | Signal | Behavior |
/// |---|---|---|
/// | 1 | binding pin `Pinned(t)` | select `t`'s slot — the multi-tenant discriminator |
/// | 2 | request-time machine pin `Pinned(t)` | select `t`'s slot — resolved NOW, not at mint |
/// | 3 | request-time machine pin `Unpinned` | the default slot (`device_bearer_for(None)`) — the legitimate single-tenant shape |
/// | 4 | request-time machine pin `Unresolvable` | **only** refuses if the device JWT ALSO carries no `tenant_id` claim |
///
/// ## `Unresolvable` KEEPS its fail-closed fallback — deliberately
///
/// Row 4 is not an oversight and must not be "simplified" into row 3.
/// Demoting `Unresolvable` to `Unpinned` would route a machine that cannot
/// state its tenant onto *whichever slot happens to exist* — which is exactly
/// the class of defect Phase 1 exists to remove, reintroduced from the other
/// end. A machine with no readable `machine.json` still has an authoritative
/// tenant, because coord stamps `tenant_id` into every device JWT it issues;
/// that claim is the second and last route, and only when BOTH miss is there
/// no tenant knowable by any route and the request is refused.
///
/// Note the refusal keys on "no tenant knowable by ANY route" rather than on
/// the file alone. Deciding priority: **robustness** — refusing too eagerly is
/// an outage on healthy machines, which is strictly worse than a silent
/// default-tenant write on a machine that is genuinely broken.
///
/// Returns the tenant to pass to [`crate::auth::device_bearer_for`] (`None`
/// meaning "the default slot"), or a typed refusal to return verbatim.
pub(crate) fn session_tenant_or_refuse(nonce: Option<&str>) -> Result<Option<Uuid>, (u16, String)> {
    use crate::session::tenant_pin::TenantPin;
    let binding_pin = nonce
        .map(proxy_session_pin_for_nonce)
        .unwrap_or(TenantPin::Unpinned);
    // THE Phase-1b read: the machine's tenant is sampled NOW, per request, not
    // recovered from whatever the binding froze at mint time.
    let live_pin = crate::session::tenant_pin::resolve_tenant_pin();
    resolve_session_tenant(binding_pin, live_pin, device_jwt_claim_tenant)
}

/// Pure-over-injected-parts core of [`session_tenant_or_refuse`], so the
/// authority order is unit-testable without touching `$HOME`, the nonce
/// registry, or the credential store.
///
/// `jwt_claim_tenant` is called at most once, and only on the arm that needs
/// it — it reads the credential store.
pub(crate) fn resolve_session_tenant(
    binding_pin: crate::session::tenant_pin::TenantPin,
    live_pin: crate::session::tenant_pin::TenantPin,
    jwt_claim_tenant: impl FnOnce() -> Option<Uuid>,
) -> Result<Option<Uuid>, (u16, String)> {
    use crate::session::tenant_pin::TenantPin;

    // Row 1. The ONE authority the binding keeps: an explicitly pinned session
    // tenant. `machine.json` names a single active tenant, so on a
    // multi-tenant device it is the only thing that can tell two co-resident
    // sessions apart. It NAMES a tenant; it no longer selects a slot family.
    if let TenantPin::Pinned(t) = binding_pin {
        if live_pin != binding_pin {
            tracing::debug!(
                "coord_mcp: session pinned to tenant {t} at mint time while this machine \
                 now reads {live_pin:?} — honoring the session's own tenant (provenance \
                 telemetry, not a credential-slot choice)"
            );
        }
        return Ok(Some(t));
    }

    // Rows 2-4. The binding carries no tenant — it is a restored nonce, an
    // adopted on-disk nonce, a single-tenant install, or a mint on a machine
    // that could not state its tenant. Whatever it was THEN is not evidence
    // about now, so resolve now.
    match live_pin {
        TenantPin::Pinned(t) => Ok(Some(t)),
        TenantPin::Unpinned => Ok(None),
        TenantPin::Unresolvable => {
            // FAIL-CLOSED, and it stays that way. Second and last route to a
            // tenant: the device JWT's own claim, which coord issued and which
            // is authoritative. NEVER fall through to `Ok(None)` here — that
            // would silently route an unresolvable device onto whichever slot
            // happens to exist, which is the Phase-1 defect wearing a
            // different hat.
            match jwt_claim_tenant() {
                Some(t) => {
                    warn!(
                        "coord_mcp: machine pin unresolvable; falling back to the \
                         device JWT's tenant claim ({t}) — repair ~/.qontinui/machine.json"
                    );
                    Ok(Some(t))
                }
                None => {
                    warn!(
                        "coord_mcp: REFUSING proxy request — tenant unresolvable by \
                         any route (no usable machine.json pin and no tenant_id claim \
                         on the device JWT)"
                    );
                    Err(tenant_unresolvable_error())
                }
            }
        }
    }
}

/// Async wrapper: resolve the session tenant AND read its bearer, or refuse.
///
/// The four `mcp_api` sites all need both halves — the tenant to hand to
/// `await_device_jwt_remint_for` on the degrade path, and the bearer itself —
/// so returning both keeps the call sites to one `await` and one `match`.
///
/// `AuthManager` does filesystem I/O and the `Unresolvable` arm reads a JWT, so
/// the whole decision runs on the blocking pool, exactly where the per-site
/// `device_bearer_for` call used to run.
///
/// A join failure (panic or cancellation) degrades to `Ok((None, None))` —
/// byte-for-byte the old `.ok().flatten()` behavior — rather than manufacturing
/// a refusal out of an executor hiccup.
pub(crate) async fn session_bearer_and_tenant_or_refuse(
    nonce: Option<String>,
) -> Result<(Option<Uuid>, Option<String>), (u16, String)> {
    match spawn_blocking_tracked(move || {
        session_tenant_or_refuse(nonce.as_deref())
            .map(|t| (t, crate::auth::device_bearer_for(t.as_ref())))
    })
    .await
    {
        Ok(res) => res,
        Err(_) => Ok((None, None)),
    }
}

/// Phase 1b tests for [`resolve_session_tenant`] — the authority order that
/// replaced "whatever the binding froze at mint time".
///
/// Hermetic: the pins and the device-JWT claim are all injected, so nothing
/// here touches `$HOME`, the nonce registry, or the credential store.
#[cfg(test)]
mod session_tenant_resolution_tests {
    use super::*;
    use crate::session::tenant_pin::TenantPin;

    fn tenant(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    /// No claim available — the closure every non-`Unresolvable` arm must not
    /// need.
    fn no_claim() -> Option<Uuid> {
        None
    }

    /// A `Pinned` binding NAMES the session's own tenant, and keeps naming it
    /// even when the machine has since moved its active tenant elsewhere. This
    /// is the one authority the pin retains: `machine.json` records a single
    /// active tenant, so nothing else can tell two co-resident sessions apart.
    #[test]
    fn a_pinned_binding_names_the_sessions_own_tenant() {
        let a = tenant(0xA1);
        let b = tenant(0xB2);
        assert_eq!(
            resolve_session_tenant(TenantPin::Pinned(a), TenantPin::Pinned(b), no_claim),
            Ok(Some(a))
        );
    }

    /// THE Phase-1b fix. A binding that carries NO tenant — a restored nonce,
    /// an adopted on-disk nonce — used to mean "the legacy `access_token`
    /// slot". It now resolves against the machine's pin AT REQUEST TIME, so a
    /// restored session presents the same credential a freshly-minted one for
    /// the same machine would.
    #[test]
    fn a_tenantless_binding_resolves_at_request_time() {
        let t = tenant(0xC3);
        assert_eq!(
            resolve_session_tenant(TenantPin::Unpinned, TenantPin::Pinned(t), no_claim),
            Ok(Some(t)),
            "a restored/adopted nonce must resolve the machine's CURRENT tenant, \
             not fall back to the default slot because of how it was created"
        );
    }

    /// THE Phase-1 acceptance test.
    ///
    /// Two bindings whose sessions belong to the SAME tenant must resolve the
    /// same credential regardless of provenance — one pinned at mint, one
    /// tenant-less and resolved live. Two bindings for DIFFERENT tenants must
    /// still resolve different ones: that is why the pin is demoted to
    /// telemetry rather than deleted outright.
    #[test]
    fn same_tenant_agrees_and_different_tenants_diverge() {
        let a = tenant(0xD4);
        let b = tenant(0xE5);

        let pinned = resolve_session_tenant(TenantPin::Pinned(a), TenantPin::Pinned(a), no_claim);
        let unpinned = resolve_session_tenant(TenantPin::Unpinned, TenantPin::Pinned(a), no_claim);
        assert_eq!(
            pinned, unpinned,
            "a Pinned and an Unpinned binding for the same tenant must resolve the \
             SAME credential"
        );
        assert_eq!(pinned, Ok(Some(a)));

        let other = resolve_session_tenant(TenantPin::Pinned(b), TenantPin::Pinned(a), no_claim);
        assert_ne!(
            pinned, other,
            "two bindings for different tenants must resolve different credentials"
        );
        assert_eq!(other, Ok(Some(b)));
    }

    /// The legitimate single-tenant shape: a readable `machine.json` that
    /// simply states no tenant. It keeps the default slot, and must not pay for
    /// a credential-store read to get there.
    #[test]
    fn an_unpinned_machine_keeps_the_default_slot_without_reading_the_store() {
        let called = std::cell::Cell::new(false);
        let claim = || {
            called.set(true);
            Some(tenant(0xFF))
        };
        assert_eq!(
            resolve_session_tenant(TenantPin::Unpinned, TenantPin::Unpinned, claim),
            Ok(None)
        );
        assert!(
            !called.get(),
            "the Unpinned arm must not reach for the device JWT's claim"
        );
    }

    /// `Unresolvable` KEEPS its fail-closed device-JWT-claim fallback: coord
    /// stamps `tenant_id` into every JWT it issues, and that claim is the
    /// second and last route to a tenant.
    #[test]
    fn unresolvable_falls_back_to_the_device_jwt_claim() {
        let t = tenant(0x11);
        assert_eq!(
            resolve_session_tenant(TenantPin::Unpinned, TenantPin::Unresolvable, || Some(t)),
            Ok(Some(t))
        );
        assert_eq!(
            resolve_session_tenant(TenantPin::Unresolvable, TenantPin::Unresolvable, || Some(t)),
            Ok(Some(t))
        );
    }

    /// …and when BOTH routes miss, it refuses — typed, not a bare 401.
    #[test]
    fn unresolvable_with_no_claim_refuses() {
        let got = resolve_session_tenant(TenantPin::Unpinned, TenantPin::Unresolvable, no_claim);
        match got {
            Err((status, body)) => {
                assert_eq!(status, 503);
                assert!(
                    body.contains("COORD_MCP_PROXY_TENANT_UNRESOLVABLE"),
                    "refusal must stay typed: {body}"
                );
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }
    }

    /// The regression this arm exists to prevent. Demoting `Unresolvable` to
    /// `Unpinned` would route a machine that cannot state its tenant onto
    /// whichever slot happens to exist — the Phase-1 defect from the other end.
    /// `Ok(None)` here would be exactly that demotion.
    #[test]
    fn unresolvable_is_never_demoted_to_unpinned() {
        assert_ne!(
            resolve_session_tenant(TenantPin::Unpinned, TenantPin::Unresolvable, no_claim),
            Ok(None),
            "an unresolvable machine must never silently select the default slot"
        );
    }

    /// A machine that has since been REPAIRED resolves normally: the binding's
    /// stale `Unresolvable` is provenance, not a verdict.
    #[test]
    fn a_repaired_machine_is_not_held_to_a_stale_unresolvable_binding() {
        let t = tenant(0x22);
        assert_eq!(
            resolve_session_tenant(TenantPin::Unresolvable, TenantPin::Pinned(t), no_claim),
            Ok(Some(t))
        );
    }
}

/// The `tenant_id` claim on whatever device JWT this runner currently holds.
///
/// Reads the legacy default slot — the only slot reachable without already
/// knowing a tenant, which is precisely the situation this is resolving. Uses
/// the existing unverified-payload decoder ([`qontinui_runner_lib::pair::tenant_id_from_oauth_claim`]);
/// signature verification is coord's job, and the value is used only to pick a
/// local credential slot, never as an authorization decision.
fn device_jwt_claim_tenant() -> Option<Uuid> {
    // The RAW slot, deliberately — NOT `device_bearer_for(None)`, which since
    // Phase 1a returns `None` for an expired or opaque token. Expiry
    // invalidates a token as a CREDENTIAL; it does not invalidate the
    // `tenant_id` coord stamped into it, and this call is picking a local
    // credential slot, never authorizing anything. Reading through the
    // validity gate here would turn a stale default slot into a hard refusal
    // on precisely the machines the fallback exists to keep working.
    let jwt = crate::auth::AuthManager::new().get_access_token().ok()?;
    let raw = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(jwt.trim())?;
    Uuid::parse_str(&raw).ok()
}

// ============================================================================
// Revocation + session-restore reaping (credential-hygiene Task 5)
// ============================================================================

/// Revoke ONE proxy nonce by value: removed from the live registry AND the
/// grace registry (revocation is total — grace only ever survives
/// supersession, never an explicit revoke), and the shrunken set is mirrored
/// to the encrypted store so the revocation survives a restart. Idempotent.
pub(crate) fn revoke_proxy_nonce(nonce: &str) {
    if nonce.is_empty() {
        return;
    }
    // Capture the revoked binding's workdir under the lock so the forensics
    // line below can name it — after the lock is released (file I/O).
    let (snapshot, revoked_workdir) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        match map.remove(nonce) {
            None => (None, None),
            Some(b) => (Some(map.clone()), Some(b.workdir)),
        }
    };
    let graced_removed = graced_nonces()
        .lock()
        .expect("graced nonce map poisoned")
        .remove(nonce)
        .is_some();
    // Rotation forensics (Phase 3): an explicit revoke is the one way a key
    // dies that leaves NO other trace — no mint, no evict, no grace. Without
    // this line a revoked nonce's later `reject`s join to nothing.
    if let Some(workdir) = &revoked_workdir {
        log_rotation_event(
            "revoke",
            workdir,
            nonce,
            "explicit revoke (live registry; grace cleared too — revocation is total)",
        );
        info!("coord_mcp: revoked proxy nonce (live registry)");
    } else if graced_removed {
        log_rotation_event(
            "revoke",
            ROTATION_UNKNOWN,
            nonce,
            "explicit revoke (grace registry only — no live binding left to name a workdir)",
        );
        info!("coord_mcp: revoked proxy nonce (grace registry only)");
    }
    if let Some(snapshot) = snapshot {
        persist_proxy_nonces(&snapshot);
    }
}

/// Revoke every proxy nonce bound to `agent_id` (live map only — agent nonces
/// are never graced nor persisted). Called at agent teardown alongside
/// [`remove_agent_token`] so a torn-down agent's nonce disappears entirely
/// instead of lingering as a permanently-401ing map entry.
pub(crate) fn revoke_agent_proxy_nonces(agent_id: Uuid) {
    // Collect (nonce, workdir) under the lock; emit the forensics lines after
    // releasing it (`log_rotation_event` does file I/O).
    let (revoked, remaining): (Vec<(String, String)>, HashMap<String, NonceBinding>) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        let mut revoked = Vec::new();
        map.retain(|n, b| {
            if b.principal == (ProxyPrincipal::Agent { agent_id }) {
                revoked.push((n.clone(), b.workdir.clone()));
                return false;
            }
            true
        });
        // Clone the surviving map for the census. Teardown does NOT go through
        // `persist_proxy_nonces` (agent nonces are never persisted), so without
        // this the newest census would keep naming bindings that are already
        // gone — and the boot readback would then classify torn-down sessions.
        // Same clone-a-snapshot idiom as `mint_and_register_nonce`, on a path
        // that fires once per agent teardown.
        (revoked, map.clone())
    };
    note_agent_binding_census(&remaining);
    for (nonce, workdir) in &revoked {
        log_rotation_event(
            "revoke",
            workdir,
            nonce,
            &format!("agent teardown (agent {agent_id} — never graced, never persisted)"),
        );
    }
    if !revoked.is_empty() {
        info!(
            "coord_mcp: revoked {} agent proxy nonce(s) for agent {agent_id}",
            revoked.len()
        );
    }
}

/// Session-close credential release for a DEVICE session's workdir: revoke the
/// workdir's registered nonce(s) and reap the app-data
/// `session-restore/coord-mcp` config file that carried the nonce. The caller
/// is responsible for the "last session in this workdir" check (two tabs can
/// share a workdir); this fn additionally refuses to touch the long-lived
/// `qontinui-root` ROOT config, whose nonce serves every root-launched session
/// and is healed at boot, not per-session.
///
/// Any graced predecessor of the revoked nonce is left to expire on its own
/// [`NONCE_GRACE_TTL`] (≤90s, DEVICE-only, process-local) — the grace registry
/// is keyed only by nonce, and a superseded nonce riding grace is already
/// evicted from the live map.
pub(crate) fn release_workdir_on_session_close(workdir: &str) {
    if workdir.trim().is_empty() {
        return;
    }
    // Never revoke the repo-root config's nonce on an individual session close
    // — it is the shared, boot-self-healed credential for ALL root sessions.
    if let Some(root) = qontinui_root_dir() {
        let same = std::fs::canonicalize(workdir)
            .ok()
            .zip(std::fs::canonicalize(&root).ok())
            .map(|(a, b)| a == b)
            .unwrap_or_else(|| Path::new(workdir) == root.as_path());
        if same {
            return;
        }
    }
    let (revoked_bindings, snapshot) = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // The terminal id rides along so the forensics line below can carry the
        // Phase 4 join key. Resolved HERE because the binding is gone from the
        // map the moment `retain` returns — a later `terminal_id_for_nonce`
        // would find nothing and report every session-close revoke as unknown.
        let mut revoked_bindings: Vec<(String, Option<String>)> = Vec::new();
        map.retain(|n, b| {
            if b.workdir == workdir {
                revoked_bindings.push((n.clone(), b.terminal_id.clone()));
                return false;
            }
            true
        });
        let snapshot = (!revoked_bindings.is_empty()).then(|| map.clone());
        (revoked_bindings, snapshot)
    };
    let revoked = revoked_bindings.len();
    // Rotation forensics: session close is the LARGEST revoke path in
    // production, and until now the only one that emitted nothing. Plan
    // 2026-08-20 Phase 3 gave `revoke_proxy_nonce` and
    // `revoke_agent_proxy_nonces` a `revoke` line and recorded this one as an
    // explicit residual ("also revokes nonces and still emits no `revoke`
    // line") — which mattered more than the two that were covered:
    // `revoke_proxy_nonce` has no production caller at all and agent nonces are
    // never persisted, so on a real box EVERY key that died by revocation died
    // here, silently. A later `reject` on one of those nonces joined to a
    // `mint`/`write` pair and then to nothing, leaving "the client is 401ing
    // and the key simply vanished" — indistinguishable from the eviction
    // cascade Phase 4 fixed, which is precisely the discrimination the
    // 2026-08-19 reconstruction needed and could not make.
    //
    // Emitted AFTER the map lock is released (the emitter does file I/O) and
    // BEFORE the persist, matching `revoke_agent_proxy_nonces`' discipline.
    for (nonce, terminal_id) in &revoked_bindings {
        log_rotation_event_with(
            "revoke",
            workdir,
            nonce,
            "session close (last open session for this workdir; live registry, \
             and the shrunken set is mirrored to the encrypted store)",
            &[(
                "terminal_id",
                serde_json::Value::from(
                    // Same spelling as the `reject` line's field: a
                    // terminal-less binding is the string "none", never JSON
                    // null, so the two lines join on one shape.
                    terminal_id.clone().unwrap_or_else(|| "none".to_string()),
                ),
            )],
        );
    }
    let revoked_nonces: Vec<String> = revoked_bindings
        .into_iter()
        .map(|(nonce, _)| nonce)
        .collect();
    if let Some(snapshot) = snapshot {
        persist_proxy_nonces(&snapshot);
    }
    // Reap every app-data --mcp-config file whose nonce we just revoked.
    //
    // Keyed on the NONCE, not on a recomputed filename. The filename is derived
    // from the terminal when there is one ([`mcp_config_file_name`]), and this
    // entry point knows only the workdir — so a name-based reap would find the
    // workdir-derived file and silently miss every per-TERMINAL one. Matching on
    // the revoked nonce set is exact for both classes and needs no terminal id
    // plumbed down here: the retain above already collected precisely the nonces
    // this close killed, and each config file carries its nonce in-band
    // ([`read_proxy_nonce`]).
    //
    // The credential was already dead the moment the retain dropped it; this is
    // hygiene, so it is best-effort and a failure only warns. The broader
    // session-restore reaper ([`reap_stale_session_restore_configs_in`]) remains
    // the backstop for files whose nonce was never registered at all.
    reap_configs_for_revoked_nonces(
        &crate::session::claude_hook::session_restore_dir().join("coord-mcp"),
        &revoked_nonces,
    );
    if revoked > 0 {
        info!("coord_mcp: session close revoked {revoked} proxy nonce(s) for {workdir}");
    }
}

/// Remove every `--mcp-config` file in `dir` whose in-band proxy nonce is in
/// `revoked`. Injectable `dir` so tests never touch the real app-data path.
///
/// Nonce-keyed rather than name-keyed on purpose — see the call site in
/// [`release_workdir_on_session_close`]: per-TERMINAL config filenames are
/// derived from the terminal id, which that entry point does not have, so
/// recomputing a name would reap only the workdir-derived file. Returns the
/// number of files removed. Best-effort throughout: an unreadable dir, an
/// unparseable file, or a failed unlink never propagates — the credential these
/// files carry is already dead by the time this runs.
fn reap_configs_for_revoked_nonces(dir: &Path, revoked: &[String]) -> usize {
    if revoked.is_empty() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(nonce) = read_proxy_nonce(&path) else {
            continue;
        };
        if !revoked.contains(&nonce) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) => warn!(
                "coord_mcp: failed to reap session-restore config {}: {e}",
                path.display()
            ),
        }
    }
    removed
}

/// True iff something is LISTENING on `127.0.0.1:port` (a short connect probe).
/// Used by the session-restore reaper to distinguish "another live runner's
/// config" (keep) from "a dead runner's leftover" (reap).
fn loopback_port_alive(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(400),
    )
    .is_ok()
}

/// Reap stale `~/.qontinui/runner/session-restore/coord-mcp/*.json` configs
/// (credential-hygiene Task 5): these files each carry a live proxy nonce and
/// were historically never deleted. Called on runner start (after the nonce
/// restore, so the registered set is authoritative) and — per-file — on
/// session close ([`release_workdir_on_session_close`]). Returns the number of
/// files removed. Decision per file:
///
/// - unparseable / not the proxy shape → reap (not a working credential file);
/// - names THIS runner's bound port → keep iff its nonce is currently
///   registered and valid, else reap (a 401-only file is pure liability);
/// - names ANOTHER port → keep iff that port is alive (a sibling/temp runner
///   shares this per-user dir and owns its own nonces), reap if dead.
pub(crate) fn reap_stale_session_restore_configs(bound_port: u16) -> usize {
    let dir = crate::session::claude_hook::session_restore_dir().join("coord-mcp");
    reap_stale_session_restore_configs_in(&dir, bound_port, &loopback_port_alive)
}

/// Injectable core of [`reap_stale_session_restore_configs`]: the directory
/// and the port-liveness probe are parameters so tests never touch the real
/// app-data dir nor open sockets.
fn reap_stale_session_restore_configs_in(
    dir: &Path,
    bound_port: u16,
    port_alive: &dyn Fn(u16) -> bool,
) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0, // absent dir → nothing to reap
    };
    let mut reaped = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let port = read_proxy_port_from(&path);
        let nonce = read_proxy_nonce(&path);
        let keep = match (port, nonce) {
            (Some(p), Some(n)) if p == bound_port => proxy_nonce_is_valid(&n),
            (Some(p), Some(_)) => port_alive(p),
            // No parseable proxy port/nonce: not a working credential file.
            _ => false,
        };
        if keep {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                reaped += 1;
                info!(
                    "coord_mcp: reaped stale session-restore config {}",
                    path.display()
                );
            }
            Err(e) => warn!(
                "coord_mcp: failed to reap stale session-restore config {}: {e}",
                path.display()
            ),
        }
    }
    reaped
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
        return Err((401, stale_proxy_key_error(STALE_PROXY_KEY_CAUSE)));
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
    spawn_blocking_tracked(move || {
        // Phase 1a: the freshness test applies to the token this actually
        // RETURNS. It used to gate on `AuthManager::device_jwt_needs_refresh()`,
        // which reads the LEGACY `access_token` slot only, and then return
        // `device_bearer_for(tenant)` — the PER-TENANT slot. A fresh legacy
        // slot therefore certified a dead per-tenant slot as "usable", which is
        // the same present-but-dead defect as `select_device_bearer`'s, one
        // layer up.
        crate::auth::device_bearer_for(tenant.as_ref())
            .filter(|t| crate::auth::slot_jwt_is_usable(t))
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

/// Can this runner deliver coord-mcp to a session it is about to spawn?
///
/// Plan: `2026-08-05-runner-memory-injection-and-tenant-fail-closed` Phase 4.
///
/// This is the **spawn-path-safe** provisioning signal: a pure in-process read
/// (Tauri app handle → managed `AppState`), no filesystem I/O, no lock held
/// across I/O, so it is legal to call from
/// [`crate::terminal::runner_context`] — which renders on every spawn and whose
/// contract forbids I/O.
///
/// It answers exactly the question [`provision_coord_mcp_config_file`] and
/// [`write_coord_mcp_proxy_config`] answer before writing anything: is the
/// bound API port resolvable? [`resolve_bound_api_port`] returns `None`
/// fail-closed when no Tauri runtime / managed state is reachable, and callers
/// "MUST treat `None` as refuse to write a proxy config" — so `false` here means
/// no session spawned now can be given a working coord-mcp.
///
/// ## Why not the `.coord-mcp-status` breadcrumb
///
/// [`COORD_MCP_STATUS_FILE`] cannot serve as a spawn-time gate. It is written by
/// [`probe_and_breadcrumb_proxy`] on a DETACHED thread specifically so it never
/// blocks provisioning, and a HEALTHY session writes nothing — so at the moment
/// a briefing is rendered, "absent" means *healthy OR not-yet-probed*, which is
/// indistinguishable and reads healthy essentially always. A gate on it would be
/// vacuous rather than conservative, and reading it would be disk I/O on the
/// spawn path besides.
pub(crate) fn coord_mcp_deliverable() -> bool {
    resolve_bound_api_port().is_some()
}

/// The on-disk nonce a device-path re-provision of `workdir` may hand to the
/// new session INSTEAD of minting (plan
/// 2026-09-02-coord-access-dies-by-eviction-not-expiry Phase F4 §2).
struct ReusableInCwdNonce {
    /// The exact nonce string `<workdir>/.mcp.json` carries — re-emitted
    /// verbatim if the file is rewritten, never rotated.
    nonce: String,
    /// The terminal its live binding names (`None` for the in-cwd class by
    /// construction — see [`write_coord_mcp_proxy_config`]). Forensics only.
    terminal_id: Option<String>,
    /// The file carries only the legacy `X-Coord-Mcp-Proxy-Key` header, so the
    /// next client launched against it would escalate a 401 into OAuth/DCR;
    /// the caller rewrites it through [`rewrite_config_preserving_nonce`].
    /// Same repair as [`RootReconcileAction::UpgradeHeaders`].
    needs_header_upgrade: bool,
}

/// Decide whether `<workdir>/.mcp.json` already carries a credential a NEW
/// session in that cwd can simply use, so the device-path provision chokepoint
/// ([`provision_coord_mcp_with_jwt`]) reuses rather than mints.
///
/// Every condition is a reason the reuse would otherwise hand the session a key
/// that is dead or about to die, or a key that is not the workdir's:
///
/// - the file must hold OUR proxy shape on the bound port — a moved port means
///   the client must reconnect anyway (the `Rewrite` rule of
///   [`root_reconcile_action`]);
/// - the nonce must be **live** via [`live_binding`], deliberately NOT
///   [`proxy_nonce_is_valid`]: that one also accepts a GRACED nonce, and a
///   graced key dies at the end of its window — handing it to a fresh session
///   would schedule that session's death;
/// - the binding must be the PERSISTENT DEVICE class: an ephemeral (mint-route)
///   nonce is TTL-bounded, so reusing it would time-bomb a runner-spawned
///   session; an agent nonce is never in an in-cwd device file, and if one were
///   (a hand-copied config) it must not be re-served — that is the principal
///   invariant, one nonce ⇒ one principal class;
/// - the binding must name THIS workdir: a config copied from another checkout
///   would otherwise make every workdir-keyed lookup answer for the wrong dir.
///
/// **What this does NOT do.** It never adopts: an on-disk nonce that is NOT in
/// the registry yields `None` and the caller mints, exactly as before — adoption
/// widens the accept set and belongs to the boot self-heal only
/// ([`adopt_on_disk_nonce`]). It never re-registers, never touches the grace
/// map, and never changes what any nonce validates as. The accept set after a
/// reuse is byte-identical to the accept set before it.
fn reusable_in_cwd_device_nonce(workdir: &str, bound_port: u16) -> Option<ReusableInCwdNonce> {
    if read_proxy_port(workdir)? != bound_port {
        return None;
    }
    let path = Path::new(workdir).join(".mcp.json");
    let nonce = read_proxy_nonce(&path)?;
    let binding = live_binding(&nonce)?;
    if binding.principal != ProxyPrincipal::Device || binding.lifetime.is_ephemeral() {
        return None;
    }
    if binding.workdir != normalize_binding_workdir(workdir) {
        return None;
    }
    Some(ReusableInCwdNonce {
        nonce,
        terminal_id: binding.terminal_id,
        needs_header_upgrade: !read_static_authorization_presence(&path),
    })
}

/// Write the DEVICE-path `.mcp.json`: an `http`-transport server pointing at
/// the runner's own loopback `/coord-mcp` proxy on the ACTUALLY-BOUND API
/// port, authenticated by a freshly-minted per-session nonce — and NO baked
/// bearer. The proxy injects a live device JWT per request, so the config
/// survives the 4h token TTL that kills static-bearer configs in sessions
/// that outlive their snapshot (the MCP client never re-reads `.mcp.json`).
///
/// Mints with `terminal_id: None`, deliberately: `<workdir>/.mcp.json` is ONE
/// file in a shared cwd, read by every session launched there, so a terminal id
/// on its binding would be a lie — it would name whichever terminal happened to
/// write the file last while serving all the others. These nonces keep the
/// workdir fallback for caller self-identification. The per-terminal key lives
/// on the app-data `--mcp-config` delivery instead
/// ([`provision_coord_mcp_config_file`]), which really is one file per terminal.
///
/// **This ALWAYS mints, and a mint evicts the cwd's prior terminal-less
/// persistent nonce.** Since Phase F4 of plan
/// 2026-09-02-coord-access-dies-by-eviction-not-expiry the session-provision
/// chokepoint ([`provision_coord_mcp_with_jwt`]) reaches this only when the
/// file holds NO live nonce ([`reusable_in_cwd_device_nonce`]); the boot
/// self-heal's `Rewrite` arm still calls it directly, on the same "port moved
/// or nothing to reuse" grounds. Do not call it to "refresh" a healthy config —
/// that is the sibling kill this plan removed.
pub(crate) fn write_coord_mcp_proxy_config(primary_wt: &str, bound_port: u16) {
    let nonce = register_proxy_nonce(primary_wt, None);
    write_mcp_json(primary_wt, &coord_mcp_proxy_config_json(bound_port, &nonce));
}

/// Rewrite `workdir`'s `.mcp.json` through the canonical producer while
/// **preserving the nonce already on disk** — no mint, no eviction, no registry
/// change at all.
///
/// The one legitimate reason to rewrite a config whose credential is fine: the
/// file predates the Phase 2 header shape and so carries only
/// `X-Coord-Mcp-Proxy-Key`, which leaves the next MCP client launched against it
/// escalating a stale-key 401 into OAuth discovery → DCR → this runner's own
/// 404 → a durable client-side poison entry. See
/// [`RootReconcileAction::UpgradeHeaders`].
///
/// Deliberately NOT [`write_coord_mcp_proxy_config`]: that mints a fresh nonce
/// and evicts the workdir's previous one, which would strand every live client
/// holding the old one — the failure this whole plan is about. Going through
/// [`coord_mcp_proxy_config_json`] keeps the emitted shape identical to a fresh
/// write (one producer, no second shape to drift), and [`write_mcp_json`] still
/// logs the `write` rotation line carrying the (unchanged) key prefix, so the
/// forensics stream shows the rewrite without showing a rotation.
///
/// Returns whether the file was ACTUALLY written (a permission failure is
/// warned about and swallowed by [`write_mcp_json`]), so a caller that reports
/// the rewrite in its own forensics line reports what happened rather than what
/// it intended.
fn rewrite_config_preserving_nonce(workdir: &str, bound_port: u16, nonce: &str) -> bool {
    write_mcp_json(workdir, &coord_mcp_proxy_config_json(bound_port, nonce))
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
/// Note what is NOT here: a baked **JWT** in `Authorization`. The proxy injects
/// a live per-request one keyed off the nonce's principal — baking a static
/// token is the failure this shape exists to avoid.
///
/// ## Why the nonce is emitted TWICE (Phase 2, plan 2026-08-20)
///
/// * `Authorization: Bearer <nonce>` is the load-bearing one. Its mere presence
///   in the STATIC headers map is what stops the MCP client attaching an OAuth
///   provider to this server, which is what stops a stale nonce's 401 escalating
///   into OAuth discovery → Dynamic Client Registration → the runner's own 404 →
///   a durable `mcpOAuth` poison entry that silences the client permanently.
///   See [`PROXY_AUTHORIZATION_HEADER_JSON`] for the measured mechanism.
/// * `X-Coord-Mcp-Proxy-Key` is kept because the consumer set for this file
///   spans three layers, only one of which this change can reach: the
///   `qontinui-claude-config` recovery doors (`/gate`, `/policy`,
///   `/coord-revive`, `/pr-status` and ~10 `.claude/commands/*.md`) read the
///   custom header out of `.mcp.json` by name, and a config that dropped it
///   would blind exactly the tooling used to diagnose this failure. Emitting
///   both makes the shape change strictly additive: nothing that reads either
///   name breaks, and the escalation is closed regardless.
///
/// Emitting both costs no new exposure — same file, same loopback-only
/// credential, same owner-only mode. (It does mean the nonce still appears in
/// cleartext in `claude --debug mcp` logs, where custom headers are printed and
/// `Authorization` comes back `[REDACTED]` — unchanged from before, not a
/// regression, and the reason a later phase may drop the custom header once the
/// config-repo layer accepts both.)
fn coord_mcp_proxy_config_json(bound_port: u16, nonce: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{bound_port}/coord-mcp"),
                "headers": {
                    (PROXY_AUTHORIZATION_HEADER_JSON): format!("{PROXY_BEARER_PREFIX}{nonce}"),
                    (COORD_MCP_PROXY_KEY_HEADER_JSON): nonce,
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
///
/// ## The principal-class marker
///
/// The emitted document carries [`COORD_MCP_PRINCIPAL_HEADER_JSON`] =
/// [`COORD_MCP_PRINCIPAL_AGENT`] — the ONE thing that distinguishes it from the
/// device shape on disk. Without it the file is byte-identical to a device
/// config, and the boot reconcile's adopt arm would re-register this
/// **agent-scoped** nonce as a Device/Persistent binding, after which
/// [`proxy_principal_for_nonce`] answers `Device` and the proxy injects the live
/// DEVICE JWT for a credential that was scoped to one agent. See
/// [`COORD_MCP_PRINCIPAL_HEADER_JSON`] for the full three-emitter table and why
/// the class is not otherwise inferable at boot.
///
/// It is a HEADER, not a sibling of `url`/`headers`, because the headers map is
/// already arbitrary and is forwarded verbatim to our own loopback route, which
/// ignores names it does not know — inert by construction rather than by
/// assumption about the client's schema validation.
pub(crate) fn write_coord_mcp_agent_proxy_config(
    primary_wt: &str,
    bound_port: u16,
    agent_id: Uuid,
) {
    let nonce = register_agent_proxy_nonce(primary_wt, agent_id);
    write_mcp_json(
        primary_wt,
        &coord_mcp_agent_proxy_config_json(bound_port, &nonce),
    );
}

/// The AGENT variant of [`coord_mcp_proxy_config_json`]: the very same document
/// plus the self-identifying principal-class marker.
///
/// Built ON TOP of the canonical producer rather than beside it, deliberately —
/// the loopback-URL / nonce-header contract still has exactly one author, so
/// the agent shape cannot drift from the device shape in any respect except the
/// one byte-range it is supposed to differ in. The marker is added to the
/// `headers` object (see [`write_coord_mcp_agent_proxy_config`]), where it is
/// invisible to [`read_proxy_nonce`], [`read_proxy_port`] and
/// [`read_static_authorization_presence`] alike — those read the credential and
/// the header shape, and the marker changes neither.
///
/// It is emphatically NOT invisible to [`existing_config_write_verdict`], and an
/// earlier version of this comment listed that function here as though it were.
/// That was the defect: invisibility is the right property for a reader asking
/// *what credential is this*, and exactly the wrong one for the guard asking
/// *is this file mine to overwrite* — for which the marker is the only evidence
/// on disk. See [`IntendedWrite`].
fn coord_mcp_agent_proxy_config_json(bound_port: u16, nonce: &str) -> serde_json::Value {
    let mut doc = coord_mcp_proxy_config_json(bound_port, nonce);
    if let Some(headers) = doc
        .pointer_mut("/mcpServers/coord-mcp/headers")
        .and_then(|h| h.as_object_mut())
    {
        headers.insert(
            COORD_MCP_PRINCIPAL_HEADER_JSON.to_string(),
            serde_json::Value::from(COORD_MCP_PRINCIPAL_AGENT),
        );
    }
    doc
}

/// Filename of the breadcrumb dropped into a session workdir when coord-mcp is
/// degraded (no JWT, unresolvable port, a failed reachability probe) **or was
/// never provisioned at all**. Referenced by the `/gate` skill + CLAUDE.md.
///
/// # The never-provisioned arm (plan
/// # 2026-08-24-headless-box-has-no-working-coord-credential-door, Phase 4)
///
/// It used to be written on DEGRADED reasons only, which made its ABSENCE
/// ambiguous: healthy, not-yet-probed, or *provisioning never ran*. That third
/// state is the one that hurt — a box sat un-provisioned with nothing
/// observable, and every reader took the absent file for health. A spawn that
/// provisions nothing now leaves [`write_unprovisioned_breadcrumb`] naming the
/// reason, so the absent-file case narrows to "healthy or not yet probed".
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
    write_status_breadcrumb(
        workdir,
        &format!("coord-mcp UNREACHABLE ({reason}) — gate registration degraded; use /gate"),
    );
}

/// Write the NEVER-PROVISIONED breadcrumb (Phase 4): this spawn gave the session
/// no coord-mcp at all, and the reason is stated rather than left to be inferred
/// from an absent file.
///
/// A DIFFERENT verdict word from [`write_degraded_breadcrumb`] on purpose.
/// "UNREACHABLE" is the verdict of an actual probe; this arm never probed
/// anything, because there was no config to probe. Collapsing the two would tell
/// a 2am reader that coord is down when in fact nothing ever asked it.
pub(crate) fn write_unprovisioned_breadcrumb(workdir: &str, reason: &str) {
    write_status_breadcrumb(
        workdir,
        &format!(
            "coord-mcp NOT PROVISIONED ({reason}) — this session was spawned with no \
             coord-mcp config; use /gate or /coord-revive"
        ),
    );
}

/// The single writer of [`COORD_MCP_STATUS_FILE`]. Best-effort: a write failure
/// only logs — losing the breadcrumb must never fail a spawn.
fn write_status_breadcrumb(workdir: &str, line: &str) {
    let path = Path::new(workdir).join(COORD_MCP_STATUS_FILE);
    if let Err(e) = std::fs::write(&path, format!("{line}\n")) {
        warn!("coord_mcp: failed to write status breadcrumb in {workdir}: {e}");
    }
}

/// Remove a stale breadcrumb once coord-mcp is confirmed reachable OR freshly
/// provisioned, so a session that recovered (e.g. a reconcile fixed the port, or
/// the next spawn in this cwd DID get a config) does not keep showing a stale
/// UNREACHABLE / NOT PROVISIONED marker. Best-effort + idempotent (absent = ok).
pub(crate) fn clear_degraded_breadcrumb(workdir: &str) {
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
        // coord-auth-exempt(not-coord): 127.0.0.1 loopback to THIS runner's own
        // coord-mcp proxy, authenticated by the per-process proxy nonce. Never leaves
        // the box; the device-JWT is what the proxy attaches upstream, not here.
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

/// Write the session's `.mcp.json` INTO the workdir (a git working tree for
/// worktree sessions).
///
/// KEPT deliberately (credential-hygiene Task 9 review, 2026-07-17): the
/// app-data `--mcp-config` delivery (`provision_coord_mcp_config_file`) is the
/// newer shape, but the in-workdir file has a hard consumer that cannot take
/// `--mcp-config`: **`qontinui-pr`** (`src/bin/qontinui_cli.rs`,
/// `find_session_mcp_config`) discovers the per-session proxy nonce + issuing
/// runner port EXCLUSIVELY by a `.mcp.json` walk-up from cwd — it is a
/// standalone CLI invoked ad hoc inside the session, with no launch seam to
/// hand it a config path (only the PORT has an env override,
/// `QONTINUI_RUNNER_API_PORT`; the nonce deliberately travels with the port in
/// the same file so they can never cross runners). Headless agent spawns and
/// the boot root/session reconcile also provision through this writer. The
/// compensating hardening: the write is owner-only (Task 6), so a leftover
/// file is at least never world-readable.
///
/// Returns whether the write SUCCEEDED. Most callers ignore it (a failed write
/// already warns, and there is no recovery), but the boot self-heal reports the
/// rewrite in its own forensics line and must not assert one that never landed.
fn write_mcp_json(primary_wt: &str, mcp_config: &serde_json::Value) -> bool {
    let mcp_path = Path::new(primary_wt).join(".mcp.json");
    match crate::fs_perms::write_owner_only(
        &mcp_path,
        serde_json::to_string_pretty(mcp_config)
            .unwrap_or_default()
            .as_bytes(),
    ) {
        Ok(()) => {
            // File only — `write_owner_only` already restricted it, and the
            // parent is a repo working-tree root that other tooling and the
            // operator must keep using, so it is deliberately left alone.
            info!("coord_mcp: wrote .mcp.json for coord-mcp in {}", primary_wt);
            // Rotation forensics (Phase 4/R6): every write of this file is a
            // client-visible rotation candidate — record which key it now
            // carries. Extraction is infallible-by-shape (the single writer
            // `coord_mcp_proxy_config_json` always sets the header); an
            // unexpected shape logs an empty prefix rather than nothing.
            // Resolved through `proxy_nonce_from_config_doc` so it survives the
            // Phase 2 shape change — hardcoding the legacy header name here
            // would log an EMPTY `key_prefix` on every write, gutting the
            // mint→write→evict join this stream exists for.
            let key = proxy_nonce_from_config_doc(mcp_config).unwrap_or_default();
            log_rotation_event(
                "write",
                primary_wt,
                &key,
                ".mcp.json rewritten (proxy shape)",
            );
            true
        }
        Err(e) => {
            warn!(
                "coord_mcp: failed to write .mcp.json in {}: {e}",
                primary_wt
            );
            false
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
            // Breadcrumb: this is the 2026-08-05 Service-token case. The session
            // otherwise learns it has no coord identity only when a claim-gated
            // tool refuses, mid-task. Name the OBSERVED sub_type so the session
            // can self-diagnose (`service` ⇒ it inherited a foreign bearer).
            write_degraded_breadcrumb(
                workdir,
                &format!(
                    "bearer sub_type={} is not device/agent — coord-mcp NOT provisioned",
                    other.unwrap_or("<absent>")
                ),
            );
            return;
        }
    }

    // The class this call is about to write, taken from the SAME `sub_type`
    // that selects the arm below — so the guard is asked about the write that
    // actually follows, not about a device write by assumption. A device bearer
    // emits the device proxy shape; every other accepted bearer (`agent`) takes
    // the agent arm.
    let intended = if sub_type.as_deref() == Some("device") {
        IntendedWrite::Device
    } else {
        IntendedWrite::Agent
    };
    if !coord_mcp_safe_to_write(workdir, intended) {
        info!(
            "coord_mcp: {workdir}/.mcp.json already holds a non-coord-mcp \
             config — leaving it untouched (no coord-mcp provisioning)"
        );
        // Breadcrumb: the shared-root / foreign-config inheritance case — the one
        // that supplied the wrong (Service) bearer on 2026-08-05.
        //
        // ONLY when the workdir ends up with no coord-mcp server at all.
        // `coord_mcp_safe_to_write` also returns false for cases that leave the
        // session BETTER off than a device provision would: an existing AGENT
        // config — either the static-bearer shape or the proxy shape carrying
        // the principal marker (the no-downgrade guard, now stated over both) —
        // and a secondary runner declining the primary's shared-root config.
        // Breadcrumbing those would drop a permanent, never-cleared
        // "UNREACHABLE" line into a session whose coord-mcp works — a false
        // alarm is worse than no breadcrumb. `workdir_declares_coord_mcp` is
        // what distinguishes them, and it is true for every one of those files.
        if !workdir_declares_coord_mcp(workdir) {
            write_degraded_breadcrumb(
                workdir,
                "workdir .mcp.json declares no coord-mcp and is not ours to rewrite — not provisioned",
            );
        }
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
        // Phase F4 (plan 2026-09-02-coord-access-dies-by-eviction-not-expiry):
        // reuse before mint. A shared canonical checkout's `.mcp.json` is read
        // by EVERY session launched there, so re-minting it evicts the nonce a
        // still-live sibling's MCP client is presenting — measured as ~95% of
        // all evictions on merytshost (finding
        // 04247382-800d-4e39-8ad3-de248f93ed0d): every gate-continuation
        // canonical-checkout fallback and every boot-restore re-spawn reached
        // this arm and minted. The per-terminal seam already skips a cwd that
        // declares coord-mcp (`terminal/session.rs`, "cwd already declares
        // coord-mcp — skipping"); this is the same check for the in-cwd path,
        // with the one extra condition the seam does not need: the on-disk
        // nonce must still be LIVE (not graced, not ephemeral, bound to this
        // cwd, on this port), or the session would be handed a dying key. A
        // mint is now the exception — no config, or a dead one — not the
        // default.
        match reusable_in_cwd_device_nonce(workdir, port) {
            Some(reuse) => {
                let rewritten = if reuse.needs_header_upgrade {
                    rewrite_config_preserving_nonce(workdir, port, &reuse.nonce)
                } else {
                    false
                };
                info!(
                    "coord_mcp: {workdir}/.mcp.json already carries a LIVE persistent \
                     device nonce on the bound port :{port} — reused (no mint, no \
                     eviction; header shape upgraded={rewritten})"
                );
                log_rotation_event_with(
                    "reuse",
                    workdir,
                    &reuse.nonce,
                    "in-cwd .mcp.json already carries a live persistent device nonce on the bound port — reused, no mint, no eviction",
                    &[
                        ("terminal_id", serde_json::Value::from(reuse.terminal_id)),
                        ("file_rewritten", serde_json::Value::from(rewritten)),
                    ],
                );
            }
            None => write_coord_mcp_proxy_config(workdir, port),
        }
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
            ..Default::default()
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
///
/// **Principal-class guard.** `intended` names the class the caller is about to
/// write, because the no-downgrade rule was always DIRECTIONAL — see
/// [`IntendedWrite`].
fn coord_mcp_safe_to_write(workdir: &str, intended: IntendedWrite) -> bool {
    let verdict = coord_mcp_write_verdict(workdir, intended);
    if verdict == McpWriteVerdict::RefusedSharedRoot {
        warn!(
            "coord_mcp: REFUSING to write {workdir}/.mcp.json — this runner is a \
             SECONDARY instance (name={:?}, port={}) and the umbrella-root \
             .mcp.json is the PRIMARY's shared state. Writing our ephemeral port \
             + nonce there would strand every root-opened session on a dead \
             endpoint once this runner exits.",
            crate::instance::instance_name(),
            crate::mcp::types::get_mcp_api_port(),
        );
    }
    if verdict == McpWriteVerdict::RefusedAgentPrincipal {
        // Warned, not silent, and deliberately unlike `RefusedExistingConfig`.
        // A foreign file is the ordinary "leave it alone" outcome; THIS is a
        // device writer being turned away from a live agent credential, which
        // is the scope-elevation attempt the marker exists to stop. An operator
        // debugging "my agent session lost coord-mcp" needs the refusal in the
        // log, not merely its absence of effect.
        warn!(
            "coord_mcp: REFUSING to write the DEVICE coord-mcp shape into \
             {workdir}/.mcp.json — the file on disk is an AGENT config \
             (either a static agent bearer, or the proxy shape carrying \
             {COORD_MCP_PRINCIPAL_HEADER_JSON}: {COORD_MCP_PRINCIPAL_AGENT}). \
             Overwriting it would hand that agent's own MCP client a DEVICE \
             credential — a scope elevation. Agent configs are (re-)written at \
             agent spawn by `write_coord_mcp_agent_proxy_config`, never by the \
             device provisioning path."
        );
    }
    verdict == McpWriteVerdict::Allowed
}

/// Why a `.mcp.json` write is allowed or refused — [`coord_mcp_safe_to_write`]'s
/// decision WITHOUT its log line.
///
/// Split out for the config report. Layer 14 exists to make the guard's refusal
/// observable, and it asked the guard directly — so opening the report on a
/// SECONDARY emitted `coord_mcp: REFUSING to write …` into the runner log the
/// operator was about to read, describing a write nobody had attempted. A
/// diagnostic that manufactures the log line it is reporting on is the same
/// class as one that materializes the directory it describes: it changes the
/// evidence by asking for it.
///
/// The report therefore calls [`coord_mcp_write_verdict`], and the WARNING —
/// which is genuinely useful when a real writer is turned away — stays with
/// [`coord_mcp_safe_to_write`], the door every writer already funnels through.
/// This is a split, not a copy: there is still exactly one implementation of the
/// rule, and the report is still reading the guard's own verdict rather than
/// re-deriving a primary/secondary test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpWriteVerdict {
    /// Writing `<workdir>/.mcp.json` is safe.
    Allowed,
    /// A secondary instance was turned away from the umbrella root — the
    /// shared-root guard. This is the arm that carries a `warn!` at the
    /// write-attempt door.
    RefusedSharedRoot,
    /// The file on disk is a foreign or unparseable config. Refused silently:
    /// this is the ordinary "leave it alone" outcome, not a misconfiguration.
    RefusedExistingConfig,
    /// The file on disk is an AGENT config and the caller intended to write the
    /// DEVICE shape — the no-downgrade guard. Split out from
    /// [`McpWriteVerdict::RefusedExistingConfig`] because it is not the ordinary
    /// leave-it-alone case: it is a refused scope elevation, it carries a
    /// `warn!`, and it is the one refusal an operator chasing a
    /// suddenly-device-scoped agent session needs to be able to grep for.
    RefusedAgentPrincipal,
}

/// Which principal class a would-be writer is about to put into `.mcp.json`.
///
/// The no-downgrade rule this feeds was ALWAYS directional — its own comment
/// says "never downgrade an existing agent JWT (richer scopes) to a device JWT"
/// — but the guard had no way to express the direction, so it asked one
/// question for both callers and answered it as though every write were a
/// device write. That was wrong in both directions at once: it refused an agent
/// path refreshing its OWN config, and (because the predicate only recognised
/// the static-bearer agent shape) it ALLOWED the device path to overwrite an
/// agent PROXY config.
///
/// That second case is the one [`COORD_MCP_PRINCIPAL_HEADER_JSON`] was added to
/// make detectable, by `58414a05d` (PR #1144) — whose own message says the
/// marker must be refused "in every direction, not only adoption, because a
/// `Rewrite` would hand the agent's own client a DEVICE credential instead".
/// It wired the marker into the two BOOT resolvers ([`reconcile_action`],
/// [`root_reconcile_action`]) and stopped there, leaving this guard — the door
/// every non-boot writer funnels through — unable to see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntendedWrite {
    /// The DEVICE loopback-proxy shape ([`write_coord_mcp_proxy_config`]) — what
    /// every reconcile arm, the identity seam and the device provisioning path
    /// emit. Refused over an existing agent config.
    Device,
    /// The AGENT loopback-proxy shape
    /// ([`write_coord_mcp_agent_proxy_config`]). An agent config is this
    /// writer's own to refresh, so an existing agent config does not refuse it.
    Agent,
}

/// The pure verdict. See [`McpWriteVerdict`] for why the log line is not here.
fn coord_mcp_write_verdict(workdir: &str, intended: IntendedWrite) -> McpWriteVerdict {
    coord_mcp_write_verdict_at(workdir, qontinui_root_dir().as_deref(), intended)
}

/// [`coord_mcp_write_verdict`] over a root the caller already resolved.
///
/// [`qontinui_root_dir`] is `workspace_paths::workspace_root()`, which reads
/// `paths.workspace_root` through `config_facade::get_setting` →
/// `settings::load_settings_full` — a WRITER (see
/// `workspace_paths::runner_workspace_root_from`). `config_report`'s layer 14
/// resolved it twice per report, once here and once in [`mcp_json_report`], so
/// opening the diagnostic could mint a `local_user_id` into the operator's
/// `settings.json`. Taking the root as an argument lets the report resolve it
/// ONCE, off the non-mutating door, and hand the same value to both.
fn coord_mcp_write_verdict_at(
    workdir: &str,
    root_dir: Option<&Path>,
    intended: IntendedWrite,
) -> McpWriteVerdict {
    if !shared_root_write_allowed_at(workdir, root_dir, crate::instance::owns_shared_root_state()) {
        return McpWriteVerdict::RefusedSharedRoot;
    }
    match existing_config_write_verdict(workdir, intended) {
        ExistingConfigVerdict::Allowed => McpWriteVerdict::Allowed,
        ExistingConfigVerdict::Foreign => McpWriteVerdict::RefusedExistingConfig,
        ExistingConfigVerdict::AgentPrincipal => McpWriteVerdict::RefusedAgentPrincipal,
    }
}

/// What the file ALREADY at `<workdir>/.mcp.json` says about a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingConfigVerdict {
    /// Absent, unreadable, or a coord-mcp config this writer owns.
    Allowed,
    /// A foreign or unparseable file — never clobber.
    Foreign,
    /// One of ours, but AGENT-class, and the caller intended the device shape.
    AgentPrincipal,
}

/// The second half of the guard: does whatever is ALREADY at
/// `<workdir>/.mcp.json` permit a rewrite of class `intended`?
fn existing_config_write_verdict(workdir: &str, intended: IntendedWrite) -> ExistingConfigVerdict {
    let path = Path::new(workdir).join(".mcp.json");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        // absent (or unreadable) → safe to create
        Err(_) => return ExistingConfigVerdict::Allowed,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&existing) {
        Ok(v) => v,
        // unparseable foreign file → do not clobber
        Err(_) => return ExistingConfigVerdict::Foreign,
    };
    match parsed.get("mcpServers").and_then(|m| m.as_object()) {
        Some(servers) => {
            if servers.len() == 1 && servers.contains_key("coord-mcp") {
                // Our own coord-mcp config — refreshable, EXCEPT never downgrade an
                // existing agent JWT (richer scopes) to a device JWT. If the current
                // bearer decodes sub_type=agent, leave it.
                //
                // **The PROXY shape now HAS an `Authorization` header too**
                // (Phase 2, plan 2026-08-20 — see
                // [`PROXY_AUTHORIZATION_HEADER_JSON`]), so the old comment here
                // ("the device-path PROXY shape has NO Authorization header, so
                // it deliberately falls through as ours-refreshable") is no
                // longer true and has been removed. What keeps the behaviour
                // correct is the SHAPE of the value, not its absence: a proxy
                // nonce is 64 hex chars and fails the JWT decode outright, so
                // `jwt_unverified_claim` yields `None` and `unwrap_or(false)`
                // still classifies a proxy config as ours-refreshable. That is
                // a real invariant rather than a coincidence — the same
                // nonce-is-never-JWT-shaped fact [`looks_like_jwt`] rests on —
                // but it is load-bearing enough to be pinned by a test
                // (`coord_mcp_safe_to_write_*` below) rather than left implicit.
                let existing_is_static_bearer_agent = parsed
                    .pointer("/mcpServers/coord-mcp/headers/Authorization")
                    .and_then(|v| v.as_str())
                    .and_then(|h| h.strip_prefix(PROXY_BEARER_PREFIX))
                    .and_then(|tok| jwt_unverified_claim(tok, "sub_type"))
                    .map(|st| st == "agent")
                    .unwrap_or(false);
                // The SECOND agent shape, and the one the JWT decode above is
                // structurally unable to see. An agent PROXY config carries a
                // 64-hex nonce in `Authorization`, not a JWT — `looks_like_jwt`
                // is false for it BY CONSTRUCTION — so `jwt_unverified_claim`
                // yields `None` and the decode classifies the file as an
                // ordinary device config we may refresh. The principal marker
                // is the only thing on disk that says otherwise, which is
                // exactly why it was added.
                let existing_is_marked_agent = config_doc_is_agent_marked(&parsed);
                let existing_is_agent = existing_is_static_bearer_agent || existing_is_marked_agent;
                match (intended, existing_is_agent) {
                    // The no-downgrade rule, in the one direction it was always
                    // about: a device write must never land on an agent config.
                    (IntendedWrite::Device, true) => ExistingConfigVerdict::AgentPrincipal,
                    // An agent writer refreshing an agent config is that
                    // writer's own file. (The trusted spawn site does not come
                    // through here at all — see `write_coord_mcp_agent_proxy_config`.)
                    _ => ExistingConfigVerdict::Allowed,
                }
            } else {
                ExistingConfigVerdict::Foreign
            }
        }
        None => ExistingConfigVerdict::Foreign,
    }
}

/// Read the loopback proxy port out of an existing coord-mcp `.mcp.json`, if the
/// file holds the PROXY shape (`url == http://127.0.0.1:<port>/coord-mcp`).
/// Returns `None` for an absent/unparseable file or a non-proxy (static-bearer)
/// shape — the latter is the agent path, which the reconcile must never touch.
fn read_proxy_port(workdir: &str) -> Option<u16> {
    read_proxy_port_from(&Path::new(workdir).join(".mcp.json"))
}

/// [`read_proxy_port`] over an explicit config-file path (the session-restore
/// reaper's files live in app-data, not at `<workdir>/.mcp.json`).
fn read_proxy_port_from(path: &Path) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let url = v
        .pointer("/mcpServers/coord-mcp/url")
        .and_then(|u| u.as_str())?;
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = rest.strip_suffix("/coord-mcp")?;
    port_str.parse::<u16>().ok()
}

// ===========================================================================
// Layer 14 of the config report — `.mcp.json`, WITHOUT its credentials.
//
// Plan `2026-08-20-effective-config-provenance-and-env-generation` Phase 4, D2.
// ===========================================================================

/// What the shared-root `.mcp.json` currently holds — SHAPE only, never
/// content.
///
/// Every field here is a fact about the file's identity, existence, ownership
/// or classification. There is deliberately no field able to carry the
/// `Authorization` bearer or the proxy nonce this file exists to deliver: the
/// same structural refusal `env_generations::EnvValue::Withheld` encodes, and
/// for the same reason — a redaction pass over rendered text is a courtesy
/// backstop, not a boundary, and this file is the single most
/// credential-dense artifact the runner writes.
///
/// The port is NOT withheld. A loopback port number is not a secret (it is in
/// `/health`, in the process table and in every log line), and it is the one
/// value that makes a stale root config diagnosable at a glance — a root file
/// naming a port that no live runner holds is precisely the "stranded on a
/// corpse" state [`coord_mcp_safe_to_write`]'s shared-root guard exists to
/// prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpJsonReport {
    /// The resolved umbrella root, or `None` when
    /// `workspace_paths::workspace_root()` yields nothing — in which case
    /// there is no shared root config on this machine at all, and that is a
    /// reading rather than a failure.
    pub(crate) root: Option<String>,
    /// `<root>/.mcp.json`. `None` exactly when `root` is.
    pub(crate) path: Option<String>,
    /// Whether that file is on disk RIGHT NOW.
    pub(crate) exists: bool,
    /// This instance's name (`QONTINUI_INSTANCE_NAME`), or `None` for the
    /// primary.
    pub(crate) instance_name: Option<String>,
    /// Whether THIS runner is the instance allowed to own shared machine-wide
    /// state — `instance::owns_shared_root_state`, which is a stronger test
    /// than "has no instance name" (a nameless secondary fails closed).
    pub(crate) owns_shared_root_state: bool,
    /// This runner's ACTUALLY-BOUND API port, for comparison against
    /// [`proxy_port`](Self::proxy_port). `None` when no Tauri runtime /
    /// managed `AppState` is reachable — see [`resolve_bound_api_port`].
    ///
    /// # Why this is not `mcp::types::get_mcp_api_port()`
    ///
    /// That function returns the **desired** port
    /// (`QONTINUI_PORT` or the `MCP_API_PORT` default). The port this runner
    /// actually LISTENS on comes from `AppState.api_port`, and
    /// `mcp_api`'s bind loop tries `[port, port+1, port+2]`, logging
    /// *"Primary port {} was blocked, using fallback port {}"* — which is the
    /// Windows zombie-socket path THIS LAYER EXISTS TO DIAGNOSE. Comparing
    /// against the desired port inverts the row in both directions on a runner
    /// that fell back to 9877: a correct `.mcp.json` naming 9877 reads as
    /// "NOT this runner's bound API port (9876)" (a false alarm on a healthy
    /// runner), and a stale one naming 9876 reads as "IS this runner's bound
    /// API port" — a false all-clear on exactly the stranded-on-a-corpse state
    /// the guard exists to prevent.
    ///
    /// `None` must therefore render as UNKNOWN. The row may never assert a
    /// port match it could not establish, and substituting the env value to
    /// get a comparison is the specific mistake this field's type forbids.
    pub(crate) this_runner_port: Option<u16>,
    /// The loopback proxy port the on-disk file currently names, when it holds
    /// the proxy shape. `None` for an absent file, an unparseable one, or the
    /// static-bearer (agent) shape.
    pub(crate) proxy_port: Option<u16>,
    /// How the file classifies — see [`McpJsonShape`].
    pub(crate) shape: McpJsonShape,
    /// Why the file could not be read, when [`exists`](Self::exists) is `true`
    /// and the read still failed (a permission denial, an exclusive lock, a
    /// non-UTF-8 payload). `None` when the read succeeded or the file is
    /// genuinely absent.
    ///
    /// Kept because "present but unreadable" and "absent" send a reader to two
    /// different places, and the OS message is the whole of the difference.
    pub(crate) read_error: Option<String>,
    /// **The guard's verdict**, taken from [`coord_mcp_write_verdict`] — the
    /// pure core [`coord_mcp_safe_to_write`] itself decides on — rather than
    /// re-derived: may this runner write the shared root config?
    ///
    /// This is the whole point of the layer. The decision was previously
    /// observable only as a `warn!` line in the runner log, so a secondary
    /// silently declining to touch root looked identical to a secondary that
    /// had never tried.
    ///
    /// The report reads the CORE and not the `warn!`-emitting wrapper on
    /// purpose: see [`McpWriteVerdict`].
    pub(crate) safe_to_write: bool,
}

/// How a `.mcp.json` classifies, by shape alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpJsonShape {
    /// No umbrella root resolved — there is no shared root config to describe.
    NoRoot,
    /// The file is not on disk. **`NotFound` only** — a read that failed for
    /// any other reason is [`Unparseable`](Self::Unparseable), because a
    /// present-but-unreadable file rendered as `absent` next to `on disk: true`
    /// is self-contradictory and sends the reader hunting the wrong fault.
    Absent,
    /// On disk and not usable: not valid JSON, no `mcpServers` object, or
    /// unreadable at all (permission denied, an exclusive lock, non-UTF-8
    /// bytes). A foreign or damaged artifact the guard refuses to clobber. The
    /// reason rides in [`McpJsonReport::read_error`].
    Unparseable,
    /// `mcpServers` is solely `coord-mcp` in the loopback PROXY shape.
    OursProxy,
    /// `mcpServers` is solely `coord-mcp` but not the proxy shape — the
    /// static-bearer (agent JWT) config the reconcile must never touch.
    OursStaticBearer,
    /// `mcpServers` holds servers other than (or in addition to) `coord-mcp` —
    /// an operator's own config.
    Foreign,
}

impl McpJsonShape {
    /// Stable wire string.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            McpJsonShape::NoRoot => "no_workspace_root",
            McpJsonShape::Absent => "absent",
            McpJsonShape::Unparseable => "unparseable_or_no_mcp_servers",
            McpJsonShape::OursProxy => "ours_proxy",
            McpJsonShape::OursStaticBearer => "ours_static_bearer",
            McpJsonShape::Foreign => "foreign",
        }
    }
}

/// Classify a `.mcp.json` document by shape, reading no value that could be a
/// credential.
///
/// Takes the PARSED document rather than a path so every arm is unit-testable
/// against a literal — the classification is the part a report can get wrong,
/// and it must not need a real umbrella root to exercise.
fn classify_mcp_json_doc(doc: &serde_json::Value) -> McpJsonShape {
    let Some(servers) = doc.get("mcpServers").and_then(|m| m.as_object()) else {
        return McpJsonShape::Unparseable;
    };
    if servers.len() != 1 || !servers.contains_key("coord-mcp") {
        return McpJsonShape::Foreign;
    }
    // Proxy shape iff the URL is our loopback proxy. Read through the same
    // `url` pointer `read_proxy_port_from` uses; nothing else about the entry
    // is touched, and in particular not `headers`.
    let is_proxy = doc
        .pointer("/mcpServers/coord-mcp/url")
        .and_then(|u| u.as_str())
        .map(|u| u.starts_with("http://127.0.0.1:") && u.ends_with("/coord-mcp"))
        .unwrap_or(false);
    if is_proxy {
        McpJsonShape::OursProxy
    } else {
        McpJsonShape::OursStaticBearer
    }
}

/// Map the result of reading `.mcp.json` to `(exists, shape, reason)`.
///
/// Pure over the `io::Result`, so the arm that matters — a file that IS on disk
/// and cannot be read — is testable without a locked file or a permission
/// fixture.
///
/// **Only `NotFound` is [`McpJsonShape::Absent`].** A permission denial, an
/// exclusive lock or a non-UTF-8 payload is a file that exists and is unusable,
/// which is exactly what [`McpJsonShape::Unparseable`] means. Folding them into
/// `Absent` produced `on disk: true; shape: absent` — a self-contradictory row
/// that sends the reader hunting a missing file instead of a locked one.
///
/// # Why `exists` comes out of THIS function and not a second `stat`
///
/// The first fix for that contradiction removed only one polarity of it. The
/// report kept taking `exists` from a separate `path.is_file()` while `shape`
/// came from here, so a `.mcp.json` that is a **directory** rendered
/// `on disk: false; … The file IS on disk and could not be read` — the same
/// self-contradiction, inverted: `is_file()` is false for a directory, while the
/// read fails with a non-`NotFound` error and lands in `Unparseable`.
///
/// Two predicates over one path can always be made to disagree, and the second
/// syscall is also a TOCTOU window (the file can be created or removed between
/// the `stat` and the read). So existence is DERIVED FROM THE SAME `io::Result`:
/// a successful read, or any failure that is not `NotFound`, means the path is
/// there. The two facts are now incapable of disagreeing because there is only
/// one observation behind them.
///
/// The reason for a PARSE failure is bounded to category + position rather than
/// `serde_json`'s Display: that Display quotes the offending token out of the
/// document, and this document carries the `Authorization` bearer and the proxy
/// nonce.
fn shape_from_read(read: std::io::Result<String>) -> (bool, McpJsonShape, Option<String>) {
    match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, McpJsonShape::Absent, None),
        Err(e) => (true, McpJsonShape::Unparseable, Some(e.to_string())),
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Err(e) => (
                true,
                McpJsonShape::Unparseable,
                Some(format!(
                    "JSON {:?} error at line {} column {}",
                    e.classify(),
                    e.line(),
                    e.column()
                )),
            ),
            Ok(doc) => (true, classify_mcp_json_doc(&doc), None),
        },
    }
}

/// Describe the shared-root `.mcp.json` for the config report.
///
/// ASKS rather than re-derives: the write verdict comes from
/// [`coord_mcp_write_verdict`] — the pure core of the guard every writer funnels
/// through — so the report and the guard cannot disagree. Re-implementing the
/// secondary/primary test here would compile, agree today, and start lying the
/// first time the guard moved.
///
/// Read-only and side-effect-free: it never mints a nonce, never writes, never
/// touches the credential slots of the document it parses, and — since the
/// verdict comes from the core rather than from [`coord_mcp_safe_to_write`] —
/// emits no `warn!` about a write nobody attempted.
///
/// # The umbrella root is INJECTED, not resolved here
///
/// [`qontinui_root_dir`] is `workspace_paths::workspace_root()`, which reads
/// `paths.workspace_root` through `config_facade::get_setting` →
/// `settings::load_settings_full` — the runner's one settings
/// writer-by-side-effect. This function needed the root TWICE (its own path, and
/// the write guard's), so a single config report entered that writer twice: on a
/// machine whose `local_user_id` is empty, opening the diagnostic minted a UUID
/// into `settings.json` and rewrote it, then reported on the file it had just
/// changed. The caller now resolves the root ONCE off the non-mutating door
/// (`workspace_paths::workspace_root_from` over an already-read `Settings`) and
/// hands the same value to both halves — which also means the report cannot
/// describe two different roots in one row.
pub(crate) fn mcp_json_report(root: Option<std::path::PathBuf>) -> McpJsonReport {
    // The ACTUALLY-BOUND port, from the same resolver `coord_doctor` uses.
    // `None` (no Tauri runtime / managed state) stays `None` all the way into
    // the row — see the field doc for why substituting
    // `mcp::types::get_mcp_api_port()` here inverts the layer's verdict.
    let this_runner_port = resolve_bound_api_port();
    let instance_name = crate::instance::instance_name();
    let owns_shared_root_state = crate::instance::owns_shared_root_state();

    let Some(root_dir) = root else {
        return McpJsonReport {
            root: None,
            path: None,
            exists: false,
            instance_name,
            owns_shared_root_state,
            this_runner_port,
            proxy_port: None,
            shape: McpJsonShape::NoRoot,
            read_error: None,
            // No shared root config exists, so the guard's own `None` arm
            // ("nothing to protect") is the honest verdict.
            safe_to_write: true,
        };
    };
    let root_str = root_dir.to_string_lossy().to_string();
    let path = root_dir.join(".mcp.json");
    // ONE observation behind both `exists` and `shape` — see `shape_from_read`
    // on why a second `is_file()` stat could contradict it (and did).
    let (exists, shape, read_error) = shape_from_read(std::fs::read_to_string(&path));

    McpJsonReport {
        root: Some(root_str.clone()),
        path: Some(path.to_string_lossy().to_string()),
        exists,
        instance_name,
        owns_shared_root_state,
        this_runner_port,
        proxy_port: read_proxy_port_from(&path),
        shape,
        read_error,
        // The verdict, WITHOUT the `warn!` its write-attempt door emits — see
        // `McpWriteVerdict`. Reporting on a refusal must not manufacture the log
        // line that records one.
        safe_to_write: coord_mcp_write_verdict_at(
            &root_str,
            Some(&root_dir),
            IntendedWrite::Device,
        ) == McpWriteVerdict::Allowed,
    }
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

/// Filename for the runner-owned coord-mcp `--mcp-config` file: a
/// non-cryptographic hash of the **`(workdir, terminal_id)` pair**. Hashing
/// keeps the name short (Windows path limits) and collision-free in practice.
///
/// **Per (workdir, TERMINAL), not per workdir alone** (plan
/// 2026-08-04-runner-caller-session-self-id-resolution Stage 1). The name is
/// STABLE across re-spawns of the same terminal in the same cwd, so a restart
/// reuses one path (rewritten with the fresh nonce) — the same stability
/// property the workdir-keyed name had, moved onto the finer key. This is what
/// makes two terminals in one cwd get two files and therefore two NONCES
/// instead of sharing one: with a single shared file, `nonce → session` is 1:N
/// and caller self-identification can never be deterministic (see
/// [`NonceBinding::terminal_id`]).
///
/// ## Why the workdir stays in the key even when a terminal is present
///
/// The nonce-registry EVICTION key is `(workdir, terminal_id)` — a persistent
/// mint evicts prior persistent nonces matching BOTH (see
/// [`register_proxy_nonce`]). Hashing the terminal ALONE made the two keys
/// disagree: a terminal re-provisioned into a different cwd (`(W1, T)` then
/// `(W2, T)`) mapped to the SAME filename, so the single file was overwritten
/// with the new nonce while the `(W1, T)` binding — a different key — was
/// never evicted. Persistent nonces have no TTL, so the superseded credential
/// stayed live and valid with no file left pointing at it: unreachable, but
/// still accepted if leaked. Keying both the same way means every file the
/// registry supersedes is a file that gets rewritten, and vice versa.
///
/// `terminal_id: None` hashes the workdir ALONE — byte-identical to the
/// original behavior, and byte-identical to the eviction rule's own
/// both-sides-`None` case — for the callers that have no terminal (the boot
/// self-heal, session-close reaping of legacy workdir-named files).
fn mcp_config_file_name(workdir: &str, terminal_id: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match terminal_id {
        // Both components, so the name moves with the eviction key. `Hash` for
        // `&str` writes a length-delimiting terminator, so ("ab", "c") and
        // ("a", "bc") do not collide.
        Some(t) => {
            workdir.hash(&mut h);
            t.hash(&mut h);
        }
        // Legacy/terminal-less shape, unchanged.
        None => workdir.hash(&mut h),
    }
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
///
/// `terminal_id` is the seam's terminal. It keys BOTH the nonce binding (making
/// caller self-identification deterministic — [`NonceBinding::terminal_id`]) and
/// the app-data filename ([`mcp_config_file_name`]), so two terminals in one cwd
/// get two files and two live nonces rather than racing one. `None` degrades to
/// the previous per-workdir behavior in full.
pub(crate) fn provision_coord_mcp_config_file(
    workdir: &str,
    terminal_id: Option<&str>,
) -> Option<std::path::PathBuf> {
    // The shared mint core (§2): fail-closed port resolve + a DEVICE, cwd-bound
    // nonce. `Persistent` = the runner-spawn class — today's semantics exactly.
    let mcp_config = mint_device_proxy_config(workdir, NonceLifetime::Persistent, terminal_id)?;
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
    // briefly world-readable inside a permissive parent. Best-effort: losing
    // coord-mcp delivery is a strictly worse outcome than a permissive dir.
    if let Err(e) = crate::fs_perms::restrict_dir_to_owner(&dir) {
        warn!(
            "coord_mcp: could not restrict {} to owner-only: {e} — \
             credential files inside may be readable by other local users",
            dir.display()
        );
    }
    let file = dir.join(mcp_config_file_name(workdir, terminal_id));
    match crate::fs_perms::write_owner_only(
        &file,
        serde_json::to_string_pretty(&mcp_config)
            .unwrap_or_default()
            .as_bytes(),
    ) {
        Ok(()) => {
            info!(
                "coord_mcp: wrote --mcp-config file {} for workdir {workdir}",
                file.display()
            );
            // Rotation forensics (Phase 4/R6): the app-data `--mcp-config`
            // materialization carries a fresh key to identity-seam sessions
            // exactly like an in-cwd `.mcp.json` write does — give it the
            // same "write" line so those sessions get the full trail.
            // Same Phase 2 shape-independence as the in-cwd writer above: the
            // key is resolved through `proxy_nonce_from_config_doc`, never by a
            // hardcoded header name, so this line never logs an empty prefix.
            let key = proxy_nonce_from_config_doc(&mcp_config).unwrap_or_default();
            log_rotation_event(
                "write",
                workdir,
                &key,
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
/// `lifetime` and `terminal_id` are the ONLY axes the two callers differ on —
/// see [`NonceLifetime`] for why the mint route's nonces are bounded and the
/// seam's are not, and [`NonceBinding::terminal_id`] for why only the seam can
/// name a terminal (the route serves sessions the runner did not spawn, so the
/// ephemeral arm below ignores the argument by construction).
fn mint_device_proxy_config(
    workdir: &str,
    lifetime: NonceLifetime,
    terminal_id: Option<&str>,
) -> Option<serde_json::Value> {
    let bound_port = resolve_bound_api_port()?;
    let nonce = if lifetime.is_ephemeral() {
        register_session_proxy_nonce(workdir)
    } else {
        register_proxy_nonce(workdir, terminal_id)
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
///
/// Passes `terminal_id: None` — this route serves BARE sessions the runner did
/// not spawn, so there is no terminal to bind (see
/// [`register_session_proxy_nonce`]).
pub(crate) fn provision_session_proxy_config(workdir: &str) -> Option<serde_json::Value> {
    mint_device_proxy_config(workdir, NonceLifetime::ephemeral(), None)
}

/// Read the per-session proxy NONCE out of an existing coord-mcp `.mcp.json`, if
/// the file holds the PROXY shape (the nonce in `Authorization: Bearer <nonce>`
/// or in the legacy `X-Coord-Mcp-Proxy-Key` header — see
/// [`proxy_nonce_from_config_doc`]). Returns `None` for an absent/unparseable
/// file or a non-proxy shape (including the static-bearer agent shape, whose
/// `Authorization` carries a real JWT). Used by the root-config self-heal: a
/// nonce no longer in the live registry (evicted on a re-provision, or simply
/// never restored) means the config would 401 the proxy, so it must be
/// rewritten even when the port still matches.
///
/// **Must accept both shapes** — reading only the legacy header would return
/// `None` for the runner's OWN freshly written config, so `resolve_root_reconcile`
/// would set `on_disk_nonce = None` / `registered = false` and
/// `root_reconcile_action` would classify it as a non-proxy shape and leave it
/// alone: boot self-heal and adopt-on-disk would go dead on exactly the configs
/// Phase 2 produces.
fn read_proxy_nonce(config_path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(config_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    proxy_nonce_from_config_doc(&v)
}

/// Boot-time reconcile decision for one session's `.mcp.json` (Phase 3c). Pure
/// over its inputs (no I/O) so the rewrite predicate is unit-testable:
///
/// - `Rewrite` — the config holds the proxy shape on a port ≠ the instance's
///   current bound port → rewrite it to the correct port (+ a fresh persisted
///   nonce) so the next MCP read targets a live proxy.
/// - `AdoptNonce` — the port already matches and a non-empty nonce is readable,
///   but that nonce is **not in the live registry** → re-register the exact
///   on-disk string, leaving the file byte-identical. The SESSION-side half of
///   [`RootReconcileAction::AdoptNonce`]; see "Why the session side needed its
///   own adopt arm" below.
/// - `UpgradeHeaders` — the port already matches, a nonce is readable AND
///   registered, but the file carries only the legacy `X-Coord-Mcp-Proxy-Key`
///   header → rewrite it in place **with that same nonce** so it gains the
///   static `Authorization` key. See [`RootReconcileAction::UpgradeHeaders`] for
///   the measured mechanism; this is the SESSION-side half of it.
/// - `Leave` — no `.mcp.json` proxy port readable, OR the port matches and the
///   file already carries the non-escalating header shape around a registered
///   nonce.
///
/// # Why the session side needed its own upgrade arm
///
/// Plan `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning` shipped the
/// header upgrade for the ROOT config only and recorded the session side as an
/// explicit residual: this resolver keyed on the port alone, so on the very
/// deploy that ships the Phase 2 emitter (runner rebuild, same port) a healthy
/// legacy-only session config classified as `Leave` and was left
/// DCR-escalating for the next client launched against it.
///
/// The residual is narrow — `acquire_for_terminal` rewrites the in-workdir file
/// through the Phase 2 emitter on **every** session spawn, so the population
/// drains on its own — but it is not benign while it lasts, for the reason the
/// plan records: `terminal/session.rs` skips `--mcp-config` injection whenever
/// [`workdir_declares_coord_mcp`] is true, so a legacy-only in-workdir file is
/// **authoritative** for a hand-launched client and is not shadowed by a
/// healthier app-data config. The exposed population is a pre-deploy worktree
/// that survives the upgrade and gets a hand-launched client before it gets a
/// runner-spawned session.
///
/// # Why the session side needed its own adopt arm
///
/// Plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs`. Before it,
/// this resolver had no `AdoptNonce`: an unregistered session nonce on the bound
/// port classified `Leave`, so the common same-port restart left EVERY non-root
/// session workdir holding a nonce the new process never registered, and the MCP
/// client that cached it 401ed forever with no in-band recovery. Measured on the
/// incident box 2026-08-25: **10 of 11 open session workdirs** held an on-disk
/// nonce the live proxy rejected; the single one that worked was the workspace
/// root — the only config the ROOT path could adopt.
///
/// The bound this arm was previously withheld for turned out not to bite: the
/// boot task feeds this resolver only the OPEN lifecycle records (see
/// [`reconcile_session_configs`]'s input contract), which measured **11 against
/// a cap of 256** ([`MAX_PERSISTED_DEVICE_NONCES`]) — not the 591 `.mcp.json`
/// files on disk. What the cap *did* need was Phase 4's age fix; see
/// [`adopt_on_disk_nonce`].
///
/// # What this deliberately does NOT do
///
/// Session adoption **never rewrites the file** — unlike the root's
/// `AdoptNonce`, which folds in the legacy-header upgrade and therefore rewrote
/// the file on the 2026-08-24 boot. The two repairs have different failure
/// modes (adoption restores a LIVE client; a header upgrade only affects the
/// NEXT client launched there) and folding them together is what made the root's
/// own `adopt` forensics line unable to claim "no file rewrite". A session
/// workdir needing both gets the adopt this boot and the header upgrade on a
/// later one, once its nonce is registered — which is exactly the `AdoptNonce`
/// → `UpgradeHeaders` sequence the arm ordering below produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconcileAction {
    Rewrite,
    /// Re-register the exact on-disk nonce into the live registry as this
    /// workdir's Device binding — **`.mcp.json` is left byte-identical**.
    /// Produced when the proxy port still matches, a non-empty nonce IS
    /// readable, but that nonce is not in the live registry (a restart the
    /// persisted set did not cover, or an eviction).
    ///
    /// "No file write" is a statement about `.mcp.json` only: the arm still
    /// appends one `adopt` line to the rotation forensics log. It does not
    /// touch the encrypted nonce store — see [`adopt_on_disk_nonce`] for why an
    /// adopted binding is never persisted.
    AdoptNonce,
    UpgradeHeaders,
    Leave,
}

/// Pure reconcile predicate for one session config: given the proxy port
/// currently written to it (`None` = absent / unparseable / not the proxy
/// shape), the nonce readable from it, whether that nonce is currently in the
/// live registry, whether its `headers` map carries a static `Authorization` key
/// at all, the instance's current bound port, and whether the file
/// self-identifies as AGENT-class ([`COORD_MCP_PRINCIPAL_HEADER_JSON`]), decide
/// what to do. See [`ReconcileAction`] for each arm's rationale.
///
/// **The ARM ORDERING mirrors [`root_reconcile_action`] exactly**, and that is
/// the point of the plan: the two resolvers answer the same question about two
/// classes of file, and every past divergence in the ordering has been a bug. In
/// particular `AdoptNonce` is tested BEFORE the header shape — a config whose
/// credential is dead and whose headers are legacy needs the credential first,
/// because the header upgrade would rewrite the file around a nonce that still
/// does not validate, and the upgrade is worthless to a client that cannot
/// authenticate at all.
///
/// # The one arm that deliberately does NOT mirror the root
///
/// The **terminal** arm differs, and always has: on a matching port with an
/// ABSENT or EMPTY nonce, [`root_reconcile_action`] falls through to `Rewrite`
/// while this resolver returns `Leave`. So a truncated / half-written session
/// `.mcp.json` sitting on the bound port is never repaired, where the
/// equivalent root file is minted fresh and rewritten.
///
/// That divergence is intended, and the asymmetry in what the two files are
/// is what makes it so:
///
/// * The ROOT config is **shared infrastructure** — one file serving every
///   session launched anywhere at or under the workspace root, with no other
///   writer that will come along and fix it. If it is unreadable, nothing
///   repairs it but this boot pass, and leaving it broken silently breaks
///   sessions that have no relationship to whatever corrupted it. Rewriting is
///   also cheap in the only currency that matters here: there is no cached
///   nonce to strand, because there was no readable nonce to cache.
/// * A SESSION config is **owned by one workdir and rewritten on every spawn**
///   (`acquire_for_terminal` → [`provision_coord_mcp_for_session`]), so the
///   population self-heals without this pass. Meanwhile the workdirs handed to
///   [`reconcile_session_configs`] come from lifecycle records, and a record can
///   name a directory whose `.mcp.json` is mid-write by a concurrent spawn, or
///   is a file we have no business minting a credential into. Writing a fresh
///   nonce there on the strength of "we could not read the old one" is a
///   guess; leaving it is not.
///
/// In short: the root's `Rewrite` fallback exists because nothing else will fix
/// it, and the session's `Leave` exists because something else routinely does.
///
/// # `is_agent_marked` short-circuits everything
///
/// A config that names itself AGENT-class is not this pass's to reconcile in
/// ANY direction. Adopting it would launder an agent-scoped nonce into a
/// Device/Persistent binding (the hazard
/// [`COORD_MCP_PRINCIPAL_HEADER_JSON`] documents in full), and rewriting it
/// would hand the agent's own MCP client a DEVICE credential in place of the
/// agent one — an elevation of that session's effective scope either way. Agent
/// configs are re-provisioned at agent spawn and are never restored across a
/// restart by design, so `Leave` costs nothing that was ever promised.
pub(crate) fn reconcile_action(
    current_proxy_port: Option<u16>,
    on_disk_nonce: Option<&str>,
    nonce_is_registered: bool,
    has_static_authorization: bool,
    bound_port: u16,
    is_agent_marked: bool,
) -> ReconcileAction {
    if is_agent_marked {
        // Self-identified AGENT class — see the doc section above. Not ours to
        // adopt, upgrade or rewrite.
        return ReconcileAction::Leave;
    }
    let Some(port) = current_proxy_port else {
        // Not our proxy shape — never touched here.
        return ReconcileAction::Leave;
    };
    if port != bound_port {
        // Port moved: a live client's cached URL is stale too, so it must
        // reconnect regardless — mint fresh + rewrite. This arm already emits
        // the current header shape, so it subsumes the upgrade, and preserving
        // the old nonce would buy nothing.
        return ReconcileAction::Rewrite;
    }
    // Port matches. Two independent stalenesses remain — the CREDENTIAL (is the
    // nonce still registered?) and the header SHAPE — and the credential wins.
    match on_disk_nonce {
        // A non-empty nonce is readable but not registered → adopt it so the
        // live client's cached nonce validates again, without a file change.
        Some(nonce) if !nonce.is_empty() && !nonce_is_registered => ReconcileAction::AdoptNonce,
        // A registered non-empty nonce → the credential is healthy; the only
        // thing left to fix is the header shape. Rewriting a shape around an
        // EMPTY credential would produce a config that authenticates against
        // nothing, which is why the emptiness check guards this arm too.
        Some(nonce) if !nonce.is_empty() && !has_static_authorization => {
            ReconcileAction::UpgradeHeaders
        }
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
    /// Healthy (port matches, the on-disk nonce is currently registered, AND the
    /// file already carries the non-escalating header shape), or not our config
    /// to touch (absent / static-bearer shape) — do nothing.
    Leave,
    /// Port matches and the on-disk nonce IS registered — the credential is
    /// perfectly healthy — but the file still carries ONLY the legacy
    /// `X-Coord-Mcp-Proxy-Key` header. Rewrite it in place **with that same
    /// nonce** so it gains the static `Authorization` key.
    ///
    /// **Why this needs its own arm.** The rest of this resolver keys on
    /// `(port, nonce, registered)` — the header SHAPE is invisible to it — so on
    /// the very deploy that ships the Phase 2 emitter (runner rebuild, same port
    /// `:9876`), a healthy legacy-only config classified as `Leave` and an
    /// unregistered one as `AdoptNonce`, and both deliberately left the file
    /// byte-identical. Either way the in-workdir `.mcp.json` kept the
    /// legacy-only shape, i.e. **still DCR-escalating for the next client
    /// launched against it** — the exact class Phase 2 exists to close, missed
    /// on every file already on disk. Runner-spawned terminals were fine (they
    /// get a fresh app-data `--mcp-config`); the residual was the in-workdir
    /// file read by the `qontinui-pr` walk-up and by hand-launched clients.
    ///
    /// It is safe precisely because the nonce does not change: a live MCP client
    /// cached that nonce at launch and never re-reads the file, so rewriting the
    /// bytes around an unchanged credential cannot strand it.
    UpgradeHeaders,
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
/// registry, whether the file's static `headers` map carries an `Authorization`
/// key at all (the DCR-escape shape — see
/// [`RootReconcileAction::UpgradeHeaders`]), the instance's bound port, and
/// whether the file self-identifies as AGENT-class
/// ([`COORD_MCP_PRINCIPAL_HEADER_JSON`]).
///
/// `is_agent_marked` short-circuits to `Leave` for the same reason it does in
/// [`reconcile_action`] — see that function's doc section. The root path is a
/// narrower exposure than the session one (the agent writer is normally aimed
/// at an agent worktree, not the umbrella root) but not a closed one:
/// [`provision_coord_mcp_for_session`]'s agent arm writes to whatever workdir it
/// is given, and an agent session opened AT the workspace root would put an
/// agent config exactly here.
pub(crate) fn root_reconcile_action(
    current_proxy_port: Option<u16>,
    on_disk_nonce: Option<&str>,
    nonce_is_registered: bool,
    has_static_authorization: bool,
    bound_port: u16,
    is_agent_marked: bool,
) -> RootReconcileAction {
    if is_agent_marked {
        // Self-identified AGENT class — never adopted (scope elevation) and
        // never rewritten (a device credential handed to an agent's client).
        return RootReconcileAction::Leave;
    }
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
        // A registered nonce → the CREDENTIAL is healthy. The file may still be
        // the legacy-only shape though, which leaves the next client launched
        // against it escalating a future 401 into OAuth/DCR — so healthy splits
        // in two on the header shape.
        Some(_) if nonce_is_registered => {
            if has_static_authorization {
                RootReconcileAction::Leave
            } else {
                RootReconcileAction::UpgradeHeaders
            }
        }
        // No nonce readable (or an empty one) → nothing to adopt; mint fresh.
        _ => RootReconcileAction::Rewrite,
    }
}

/// Resolve the `qontinui-root` directory the checked-in repo-root `.mcp.json`
/// lives in.
///
/// This was a hand-inlined mirror of `agent_runtime::qontinui_root_dir`,
/// duplicated so this leaf module did NOT depend on `agent_runtime` (which
/// depends back on us — the cycle this module was extracted to break). Phase 2
/// of `2026-08-04-remove-hardcoded-machine-paths-from-product-code` dedupes it
/// **without** reintroducing that cycle: [`crate::workspace_paths`] is itself a
/// leaf, depending only on the settings store and the shared
/// `qontinui_types::paths` — never on `agent_runtime`.
fn qontinui_root_dir() -> Option<std::path::PathBuf> {
    crate::workspace_paths::workspace_root()
}

/// Re-register an EXISTING on-disk proxy nonce string into the live registry as
/// a Device binding for `workdir`, WITHOUT minting a new nonce
/// (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart, Change 1).
/// This is the restart-resilient self-heal: when the root `.mcp.json` proxy port
/// still matches but its nonce was evicted / never restored, adopting the exact
/// on-disk string keeps a live MCP client's CACHED nonce validating across the
/// restart (the client never re-reads the file, so a fresh-minted-and-rewritten
/// nonce would strand it on a 401). Evicts any prior nonce for the same workdir
/// (there should be none).
///
/// # An adopted binding is NEVER persisted
///
/// This used to call [`persist_proxy_nonces`], so that "the adoption itself
/// survives the NEXT restart". That was buying nothing and costing the one
/// thing that matters here.
///
/// Buying nothing: adoption is idempotent and **fully re-derivable from disk on
/// every boot**. The `.mcp.json` still holds the nonce next time, the port
/// comparison still matches, and the resolver still yields `AdoptNonce` — the
/// registry entry is reconstructed at the next boot from the same file it was
/// reconstructed from at this one. Persisting it merely writes down an answer
/// the next boot recomputes anyway.
///
/// Costing: what the encrypted store confers is **durability on a credential of
/// UNKNOWN provenance**. Every emitter of the proxy `.mcp.json` writes a
/// byte-identical document
/// ([`coord_mcp_proxy_config_json`]) and three of them mint three different
/// classes — Device/Persistent, Agent-scoped, and Device/Ephemeral — of which
/// the latter two are *guaranteed* unregistered after a restart and so are
/// exactly what the adopt predicate selects for. The marked ones are refused
/// upstream ([`COORD_MCP_PRINCIPAL_HEADER_JSON`]); an UNMARKED legacy one is
/// still not attestable. Adopting such a nonce for this process's lifetime is a
/// bounded, self-limiting repair. Writing it into the store is what would make
/// it eternal — restored on every subsequent boot with no file left to justify
/// it.
///
/// The `minted_at` argument is NOT made pointless by this: the live binding
/// carries it, so any snapshot a LATER mint triggers orders this binding by the
/// `.mcp.json`'s real age rather than by the adoption instant — which is the
/// whole of what Phase 4 exists to establish.
///
/// DEVICE binding only — and that is precisely why the caller must have refused
/// a marked agent config first. (A static-bearer agent shape cannot reach here
/// at all: it has no proxy URL, so [`read_proxy_port`] returns `None` and the
/// resolver leaves it. The AGENT-PROXY shape is the one that can, and the
/// marker is what stops it.)
///
/// `file_rewritten` says whether the caller ALREADY rewrote `.mcp.json` while
/// adopting — which the `AdoptNonce` arm of [`reconcile_root_config_at`] does
/// whenever the on-disk file still carries the legacy header-only shape (the
/// nonce is re-emitted verbatim, so the cached-nonce contract is untouched;
/// what changes is the header shape the NEXT client reads). It exists only to
/// keep the `adopt` forensics line's cause TRUE: this function used to assert
/// "no file rewrite" unconditionally, which became the opposite of what
/// happened the moment the header upgrade was folded into that arm. A cause
/// string that contradicts the action is worse than no cause string — the
/// adjacent `write` line makes the stream reconstructable, but only if the
/// reader does not trust the cause.
///
/// `minted_at` is the age the adopted binding carries into the registry, and it
/// must come from something REAL — [`config_mtime_or_epoch`] over the very
/// `.mcp.json` the nonce was read from, sampled BEFORE any rewrite this arm
/// performs. Plan
/// `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 4: this used
/// to stamp `SystemTime::now()` and then persist unconditionally,
/// while [`device_nonce_snapshot`] cuts the persisted set
/// NEWEST-first. Every adopted binding therefore sorted to the head and survived
/// the [`MAX_PERSISTED_DEVICE_NONCES`] cut ahead of restored bindings carrying
/// their true persisted age ([`minted_at_from_unix`]) — adoption INVERTED the
/// age ordering the persisted-age work exists to establish. Harmless while the
/// eligible set is 11 against a cap of 256; wrong the moment a long-lived
/// install approaches the cap, which the constant's own doc-comment says an
/// operator WILL meet. `UNIX_EPOCH` (which already sorts oldest) is the correct
/// value when the mtime is unreadable — the honest "age unrecoverable", never
/// laundered into "minted just now".
fn adopt_on_disk_nonce(
    workdir: &str,
    nonce: &str,
    file_rewritten: bool,
    minted_at: std::time::SystemTime,
) {
    let evicted = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // Persistent AND terminal-less only — an adopted nonce came from a
        // runner-written `.mcp.json`, and must NOT evict a bare session's
        // ephemeral nonce for the same workdir (the class-scoping rationale in
        // `mint_and_register_nonce`) nor a live TERMINAL's per-terminal nonce
        // for it (the same rationale applied to the terminal key: the adopted
        // nonce replaces the shared `.mcp.json` credential, which is the
        // terminal-less one). Byte-for-byte the previous behavior before
        // per-terminal nonces existed, when every persistent binding was
        // terminal-less.
        let mut evicted: Vec<String> = Vec::new();
        map.retain(|n, b| {
            if b.workdir == workdir && b.terminal_id.is_none() && !b.lifetime.is_ephemeral() {
                evicted.push(n.clone());
                return false;
            }
            true
        });
        map.insert(
            nonce.to_string(),
            NonceBinding {
                // Phase 3c: the third and last entry point into the map.
                workdir: normalize_binding_workdir(workdir),
                principal: ProxyPrincipal::Device,
                lifetime: NonceLifetime::Persistent,
                // PROVENANCE TELEMETRY (Phase 1d), same as the restore path
                // above. A `.mcp.json` stores only URL + nonce, so the session's
                // own tenant is unrecoverable; what IS knowable is the machine's
                // pin at adopt time, and since Phase 1b stripped this field of
                // its authority over credential selection, recording that is
                // honest rather than load-bearing. The bearer for an adopted
                // nonce is resolved at request time by
                // `session_tenant_or_refuse`, exactly as for a freshly-minted
                // one.
                //
                // `principal: ProxyPrincipal::Device` above is UNCHANGED and
                // must stay that way — `58414a05d` hardened the emitter side so
                // an agent-scoped config is not adoptable as Device, and this
                // field is the consumer half of that pair.
                session_pin: crate::session::tenant_pin::resolve_tenant_pin(),
                // Same reason for the terminal: a `.mcp.json` carries only URL
                // + nonce, so the terminal the file was originally provisioned
                // for is unrecoverable — and that terminal's PTY died with the
                // previous runner anyway. Caller self-identification falls back
                // to the workdir leg for an adopted nonce.
                terminal_id: None,
                // NOT `now()` — see this function's doc comment. The age comes
                // from the `.mcp.json` the nonce was read from, so an adopted
                // binding never outranks a genuinely newer one in the persisted
                // set's newest-first cut.
                minted_at,
            },
        );
        evicted
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
        if file_rewritten {
            "re-registered the on-disk `.mcp.json` nonce (file REWRITTEN to \
             upgrade the legacy header shape; same nonce re-emitted verbatim)"
        } else {
            "re-registered the on-disk `.mcp.json` nonce (no file rewrite)"
        },
    );
    // NO `persist_proxy_nonces` — see this function's doc comment. An adopted
    // binding is re-derived from the same `.mcp.json` on the next boot; putting
    // it in the encrypted store would only make a credential of unattestable
    // provenance outlive the file that justified it.
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
        read_static_authorization_presence(&path),
        bound_port,
        read_agent_principal_marker(&path),
    );
    (action, on_disk_nonce)
}

/// Whether the `.mcp.json` at `config_path` carries a static `Authorization`
/// key in its `coord-mcp` headers map. An unreadable / unparseable file is
/// `false`, matching every other reader here (and harmlessly: a file that
/// cannot be read cannot be classified as a healthy proxy config either, so the
/// answer is never load-bearing on its own).
fn read_static_authorization_presence(config_path: &Path) -> bool {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| config_doc_has_static_authorization(&v))
        .unwrap_or(false)
}

/// Whether the `.mcp.json` at `config_path` self-identifies as AGENT-class —
/// the [`COORD_MCP_PRINCIPAL_HEADER_JSON`] marker
/// [`write_coord_mcp_agent_proxy_config`] stamps and nothing else emits.
///
/// An unreadable / unparseable / absent file is `false`, matching every other
/// reader here. **`false` is not a device-class attestation** — it is "this file
/// does not vouch for itself", which is also what every config written before
/// the marker existed says. It is safe as the input to a REFUSAL (a marked file
/// is definitely not ours to touch) and would be unsafe as the input to a
/// permission; nothing here treats it as the latter.
fn read_agent_principal_marker(config_path: &Path) -> bool {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| config_doc_is_agent_marked(&v))
        .unwrap_or(false)
}

/// The `.mcp.json`'s last-modified time, or `UNIX_EPOCH` when it is unreadable.
///
/// This is the age [`adopt_on_disk_nonce`] gives an adopted binding (plan
/// `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 4). It is
/// the closest honest proxy available for when that nonce was minted: the file
/// was written by the very act of provisioning the nonce, so its mtime is that
/// mint's timestamp unless something later rewrote the file — and a rewrite
/// would have carried a fresh nonce anyway.
///
/// `UNIX_EPOCH` on failure is deliberate and matches [`minted_at_from_unix`]'s
/// unknown-age sentinel: it sorts OLDEST in [`device_nonce_snapshot`]'s
/// newest-first cut, so an unreadable age is dropped from the persisted set
/// before any dated binding. Falling back to `now()` would do the opposite —
/// promote the least-known entry to the head — which is the exact inversion
/// Phase 4 exists to remove.
fn config_mtime_or_epoch(config_path: &Path) -> std::time::SystemTime {
    std::fs::metadata(config_path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

/// True iff the root `.mcp.json` at `root_dir` needs SOME self-heal action —
/// adopt-nonce, header-shape upgrade, or full rewrite — for the current
/// `bound_port`. Returns `false` (leave
/// it) for an absent file, a non-proxy (static-bearer) shape, or a proxy config
/// whose port matches AND whose nonce is currently registered AND whose headers
/// already carry the static `Authorization` shape. Delegates to the
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
/// - `UpgradeHeaders` — port unchanged AND the on-disk nonce is registered, but
///   the file carries only the legacy proxy-key header: rewrite it through
///   [`rewrite_config_preserving_nonce`], **preserving the nonce
///   verbatim**, so the next client launched against it stops escalating a 401
///   into OAuth/DCR. Port and nonce both unchanged; only the headers map moves.
/// - `AdoptNonce` — port unchanged, a nonce IS on disk but unregistered:
///   re-register that EXACT string ([`adopt_on_disk_nonce`]) so a live client's
///   cached nonce keeps validating. The file is **also rewritten when it still
///   carries the legacy header-only shape** — the same upgrade as the arm
///   above, folded in because an adopted config was by definition written by an
///   older runner and is the likeliest legacy shape on the box. The nonce is
///   re-emitted verbatim either way, so this is never a rotation; when the
///   shape is already current the file is left byte-identical.
/// - `Rewrite` — port moved (client must reconnect regardless) or no nonce
///   readable: mint fresh + rewrite via [`write_coord_mcp_proxy_config`]. The
///   only arm that rotates the nonce.
/// - `Leave` — healthy or not ours. Nothing is written.
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
        RootReconcileAction::UpgradeHeaders => {
            if !coord_mcp_safe_to_write(&root, IntendedWrite::Device) {
                return RootReconcileAction::Leave;
            }
            // SAFETY: the resolver only yields UpgradeHeaders when a registered
            // nonce was read from disk, so this Option is always Some here.
            let nonce = on_disk_nonce
                .expect("UpgradeHeaders implies a readable on-disk nonce (resolver invariant)");
            rewrite_config_preserving_nonce(&root, bound_port, &nonce);
            info!(
                "coord_mcp: boot self-heal UPGRADED the header shape of root \
                 {root}/.mcp.json (port :{bound_port} and nonce BOTH unchanged) — the \
                 file carried only the legacy proxy-key header, which leaves the next \
                 client launched against it escalating a 401 into OAuth/DCR"
            );
            RootReconcileAction::UpgradeHeaders
        }
        RootReconcileAction::AdoptNonce => {
            if !coord_mcp_safe_to_write(&root, IntendedWrite::Device) {
                return RootReconcileAction::Leave;
            }
            // SAFETY: the resolver only yields AdoptNonce when a non-empty nonce
            // was read from disk, so this Option is always Some here.
            let nonce = on_disk_nonce
                .expect("AdoptNonce implies a readable on-disk nonce (resolver invariant)");
            // Same header-shape upgrade as the arm above, folded in here rather
            // than deferred: an adopted config is by definition one that was
            // written by an OLDER runner, so it is the most likely legacy-only
            // shape on the box, and deferring the upgrade to the next boot would
            // leave it DCR-escalating for a whole runner lifetime. The adopted
            // nonce is re-emitted verbatim, so the "live client's cached nonce
            // keeps validating" contract is untouched — what changes is the file
            // the NEXT client reads.
            //
            // The rewrite runs BEFORE the adopt so `adopt_on_disk_nonce` can
            // state in its forensics cause what actually happened to the file,
            // not what was about to be attempted. Nothing is racing in between:
            // the file already held this exact nonce, so the interval is a
            // no-op for any reader.
            let config_path = root_dir.join(".mcp.json");
            // Sample the age BEFORE the upgrade rewrite — the rewrite would
            // stamp the file `now()` and hand the adopted binding exactly the
            // adoption-instant age Phase 4 removed
            // (`2026-08-25-boot-adopt-session-nonces-across-all-workdirs`). The
            // nonce being adopted was minted when the file was LAST written by a
            // provisioning path, and that is the moment this reads.
            let minted_at = config_mtime_or_epoch(&config_path);
            let needs_upgrade = !read_static_authorization_presence(&config_path);
            let rewritten =
                needs_upgrade && rewrite_config_preserving_nonce(&root, bound_port, &nonce);
            adopt_on_disk_nonce(&root, &nonce, rewritten, minted_at);
            info!(
                "coord_mcp: boot self-heal ADOPTED on-disk root nonce for {root} \
                 (port :{bound_port} unchanged; nonce preserved verbatim; header shape \
                 needed_upgrade={needs_upgrade}, file_rewritten={rewritten}) — live \
                 MCP client cache preserved"
            );
            RootReconcileAction::AdoptNonce
        }
        RootReconcileAction::Rewrite => {
            if !coord_mcp_safe_to_write(&root, IntendedWrite::Device) {
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
        // only ever yields Leave/UpgradeHeaders/AdoptNonce/Rewrite —
        // `SkippedSecondary` is
        // produced solely by the instance gate in [`reconcile_root_config_gated`],
        // which returns BEFORE calling this function. Handle it as identity rather
        // than `unreachable!` so a future resolver change degrades to a no-op skip
        // instead of a panic on the boot path.
        RootReconcileAction::SkippedSecondary => RootReconcileAction::SkippedSecondary,
    }
}

/// What one boot-time session-config reconcile pass actually did. THREE counts,
/// not one, because the three effects are different events for an operator: a
/// `rewritten` config ROTATED its nonce (any live client holding the old one
/// must reconnect), an `upgraded` one kept its nonce byte-for-byte and only
/// gained the static `Authorization` key, and an `adopted` one was not written
/// at all — only the registry moved. Collapsing them into a single "rewrote N
/// configs" boot line — which is what this returned before the upgrade arm
/// existed — would report a harmless shape repair, or a pure registry
/// re-registration, in the same words as a credential rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SessionReconcileCounts {
    /// Configs rewritten to the current bound port with a FRESH nonce.
    pub(crate) rewritten: usize,
    /// Configs rewritten in place preserving their existing nonce, purely to
    /// add the static `Authorization` header.
    pub(crate) upgraded: usize,
    /// Configs whose on-disk nonce was on the bound port but NOT in the live
    /// registry, and was therefore re-registered verbatim — **no file write**.
    ///
    /// This is also Phase 1's blast-radius measurement (plan
    /// `2026-08-25-boot-adopt-session-nonces-across-all-workdirs`): the adopt
    /// arm cannot fail, so this number is exactly "how many enumerated session
    /// workdirs held a proxy-shaped `.mcp.json` whose nonce the live registry
    /// would have rejected". On the incident box that number was 10 (of 11 open
    /// workdirs); before the arm existed every one of them classified `Leave`
    /// and the boot line reported `rewrote 0 session config(s)` — "nothing
    /// needed doing" in the words of "nothing was repairable".
    ///
    /// It counts only the OPEN-record set the boot task passes in (see
    /// [`reconcile_session_configs`]'s input contract) — never the whole-disk
    /// `.mcp.json` census, which is ~54x larger and mostly unreachable from
    /// here.
    pub(crate) adopted: usize,
    /// Configs carrying the AGENT principal marker whose repair the guard
    /// turned away — i.e. those for which the resolver would have returned
    /// something other than `Leave` had the marker been absent.
    ///
    /// A count, not merely the per-file `warn!`, because the warn is the only
    /// record today and a per-file line does not answer the question an
    /// operator actually asks after a restart: *how many* agent sessions are
    /// sitting on a credential this boot deliberately did not repair. Zero is
    /// the expected steady state, so a non-zero number here is the signal that
    /// agent sessions outlived the previous runner and need re-spawning.
    ///
    /// Counted only for workdirs where a repair was genuinely refused — an
    /// agent config that was already healthy (resolver: `Leave` either way)
    /// does not count, for the same reason it does not warn.
    pub(crate) refused_agent_marked: usize,
}

/// Boot-time session-config reconcile (Phase 3c). For each live session workdir,
/// dispatch on [`reconcile_action`]:
///
/// * port ≠ the instance's CURRENT bound port → rewrite via
///   [`write_coord_mcp_proxy_config`] (correct port + a freshly persisted
///   nonce), so the next MCP read targets a live proxy;
/// * port matches but the on-disk nonce is NOT in the live registry → adopt it
///   through [`adopt_on_disk_nonce`] with `file_rewritten = false`, so a live
///   MCP client's cached nonce validates again against a **byte-identical**
///   file. This is the session-side half of the root path's
///   [`RootReconcileAction::AdoptNonce`] and the whole point of plan
///   `2026-08-25-boot-adopt-session-nonces-across-all-workdirs`;
/// * port matches, the nonce is registered, but the file carries only the legacy
///   proxy-key header → rewrite in place through
///   [`rewrite_config_preserving_nonce`], **preserving the nonce verbatim**, so
///   the next client launched against it stops escalating a 401 into OAuth/DCR.
///   This is the session-side half of the root path's
///   [`RootReconcileAction::UpgradeHeaders`], and closes the residual plan
///   `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning` recorded
///   against this function.
///
/// Both **mutating** arms are guarded by [`coord_mcp_safe_to_write`] so neither
/// clobbers an agent-spawn's static-bearer config. The adopt arm writes nothing,
/// so it needs no write guard — and cannot need one: refusing to write is not a
/// reason to refuse to re-register a credential the file already carries.
/// Combined with Phase 3b (persisted nonces), the common same-port restart
/// still rotates nothing.
///
/// # Input contract: `workdirs` is the OPEN-lifecycle-record set
///
/// The sole production caller (the boot reconcile task in
/// `mcp_api::start_server`) passes exactly
/// `store.open_records().filter_map(|r| r.working_dir)`, and
/// [`crate::session::session_lifecycle_store::SessionLifecycleStore::open_records`]
/// filters `state == "open"`. **That is a precondition, not a coincidence, and
/// it is the reason this function contains no open-record filter of its own.**
///
/// Plan `2026-08-25-…` originally proposed adding one — "adopt only for workdirs
/// with an open lifecycle record" — and vetting struck it as a no-op that
/// restated its own precondition. The measurement that settled it, taken on the
/// incident box 2026-08-25 over the 591 `.mcp.json` files under the workspace
/// root: **11 backed by an open record, 2 by a closed one, 578 orphaned with no
/// lifecycle record at all.** The 580 unreachable ones are never passed in. A
/// filter here would have looked like a bound on the adopted set while bounding
/// nothing, and the real bound — that the adopted set stays far under
/// [`MAX_PERSISTED_DEVICE_NONCES`] — is a consequence of this contract.
///
/// ## What widening the caller would actually admit — read this first
///
/// The count is the SECOND-order problem. The first is **principal class**.
///
/// Every emitter of the loopback proxy `.mcp.json` funnels through
/// [`coord_mcp_proxy_config_json`] and produces a byte-identical document, but
/// the nonces inside them are three different security classes:
/// [`write_coord_mcp_proxy_config`] mints Device/Persistent,
/// [`write_coord_mcp_agent_proxy_config`] mints one scoped to a single AGENT,
/// and [`provision_session_proxy_config`] mints Device/**Ephemeral** (a TTL, and
/// an opt-in marker that can revoke it). The latter two are never persisted and
/// are **guaranteed** unregistered after a restart — which is precisely the
/// predicate the adopt arm keys on — while [`adopt_on_disk_nonce`] registers
/// whatever it is handed as Device. Adopt an agent config and
/// [`proxy_principal_for_nonce`] starts answering `Device` for it, so the proxy
/// injects the live DEVICE JWT for a credential scoped to one agent. Adopt an
/// ephemeral one and its TTL and kill switch are gone.
///
/// What keeps this closed today is a pair of narrow facts, and BOTH are
/// properties of the caller, not of this function:
///
/// * `open_records()` names session workdirs the runner is tracking as live,
///   which is not where the agent and ephemeral emitters normally write; and
/// * the agent emitter now stamps a principal-class marker
///   ([`COORD_MCP_PRINCIPAL_HEADER_JSON`]) that the resolver refuses outright —
///   but only configs written by a runner carrying that change say anything at
///   all. **Unmarked is not device-class**; it is silence.
///
/// A disk census would hand this function 578 files of entirely unattested
/// provenance, including every agent worktree and every bare-session mint that
/// ever touched the box, and the adopt arm would classify each one Device.
///
/// The count matters too, and second: 578 adoptions against a cap of 256
/// ([`MAX_PERSISTED_DEVICE_NONCES`]) is a different program from 11 against 256
/// — though note that adoption itself no longer persists
/// ([`adopt_on_disk_nonce`]), so what the cap now governs is the mints these
/// adoptions share a live map with, not the adoptions themselves.
///
/// So: widening the caller is a **security** change first and a capacity change
/// second. Re-derive the class story before the count.
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
pub(crate) fn reconcile_session_configs<I>(workdirs: I, bound_port: u16) -> SessionReconcileCounts
where
    I: IntoIterator<Item = String>,
{
    let mut counts = SessionReconcileCounts::default();
    for workdir in workdirs {
        let config_path = Path::new(&workdir).join(".mcp.json");
        // Port, nonce and header shape, read the way `resolve_root_reconcile`
        // reads them for the root file — three reads of one path, not one
        // parse shared between them. That is the existing shape and it is left
        // alone deliberately, but it is worth being exact about what it does
        // NOT buy: the reads are not atomic with each other, so a rewrite
        // landing between them could hand the resolver a mixed observation.
        // Harmless here in both directions — the only writer that races this
        // is a session spawn, which writes the CURRENT shape, so the worst
        // case is an `UpgradeHeaders` that re-emits the same nonce over a file
        // that just gained the header anyway. Idempotent, and the nonce is
        // preserved either way.
        let on_disk_nonce = read_proxy_nonce(&config_path);
        // Registration is read from the LIVE registry the same way
        // `resolve_root_reconcile` reads it for the root file — after the boot
        // restore has run, so a nonce the persisted set brought back is
        // correctly seen as registered and is NOT re-adopted.
        let nonce_is_registered = on_disk_nonce
            .as_deref()
            .map(proxy_nonce_is_valid)
            .unwrap_or(false);
        // The PRINCIPAL-CLASS guard. Three emitters produce a byte-identical
        // proxy `.mcp.json` and mint three different classes of nonce; only the
        // marker distinguishes them on disk. See
        // `COORD_MCP_PRINCIPAL_HEADER_JSON`.
        let is_agent_marked = read_agent_principal_marker(&config_path);
        let current_proxy_port = read_proxy_port(&workdir);
        let has_static_authorization = read_static_authorization_presence(&config_path);
        let action = reconcile_action(
            current_proxy_port,
            on_disk_nonce.as_deref(),
            nonce_is_registered,
            has_static_authorization,
            bound_port,
            is_agent_marked,
        );
        if is_agent_marked {
            // Name what was refused, not merely that something was skipped —
            // and only when something actually WAS refused, so a fleet of
            // healthy agent workdirs does not warn on every boot. Re-running the
            // pure resolver without the guard is free and says exactly which
            // repair the marker turned away.
            let refused = reconcile_action(
                current_proxy_port,
                on_disk_nonce.as_deref(),
                nonce_is_registered,
                has_static_authorization,
                bound_port,
                false,
            );
            if !matches!(refused, ReconcileAction::Leave) {
                counts.refused_agent_marked += 1;
                warn!(
                    "coord_mcp: boot reconcile REFUSED to {refused:?} \
                     {workdir}/.mcp.json — it carries the AGENT principal marker \
                     ({COORD_MCP_PRINCIPAL_HEADER_JSON}: {COORD_MCP_PRINCIPAL_AGENT}), so \
                     its nonce is scoped to ONE agent's JWT. Adopting it would re-register \
                     that credential as Device/Persistent and the proxy would then inject \
                     the DEVICE JWT for it; rewriting it would hand the agent's own client \
                     a device credential. Agent configs are re-provisioned at agent spawn \
                     and are never restored across a restart by design — this file is not \
                     the boot reconcile's to repair."
                );
            }
        }
        match action {
            ReconcileAction::Leave => continue,
            ReconcileAction::AdoptNonce => {
                // NO `coord_mcp_safe_to_write` guard, and none is possible.
                //
                // Be exact about what "writes nothing" means, because the loose
                // version of this sentence is what hid a scope-elevation hazard
                // for a whole review cycle. This arm writes nothing **to
                // `.mcp.json`** — the file is left byte-identical, and that is
                // the only property the guard is about (the guard exists to
                // stop us clobbering a file that is not ours). It DOES append
                // one `adopt` line to the rotation forensics log, and it mutates
                // the in-process nonce registry. It no longer touches the
                // encrypted store at all: `adopt_on_disk_nonce` deliberately
                // does not persist, so an adopted binding lives exactly one
                // process lifetime and is re-derived from this same file at the
                // next boot.
                //
                // (An agent STATIC-BEARER config cannot reach here: it has no
                // proxy URL, so `read_proxy_port` returns `None` and the
                // resolver leaves it. An agent PROXY config could — it is
                // byte-identical to the device shape apart from the
                // principal-class marker — which is what the marker check
                // above exists to stop.)
                //
                // SAFETY: the resolver only yields AdoptNonce when a non-empty
                // nonce was read from disk — the same invariant the root path
                // relies on.
                let nonce = on_disk_nonce
                    .expect("AdoptNonce implies a readable on-disk nonce (resolver invariant)");
                // `file_rewritten = false`, and the header upgrade is
                // deliberately NOT folded in here the way the root arm folds it
                // (plan `2026-08-25-…` Phase 2, decided during vetting). The two
                // repairs have different failure modes, and folding them is what
                // left the root's own `adopt` forensics line claiming a rewrite.
                // A legacy-shaped config gets its upgrade on a LATER boot, once
                // this adoption has made its nonce registered.
                adopt_on_disk_nonce(&workdir, &nonce, false, config_mtime_or_epoch(&config_path));
                counts.adopted += 1;
                info!(
                    "coord_mcp: boot reconcile ADOPTED the on-disk nonce of session \
                     {workdir}/.mcp.json (port :{bound_port} unchanged; file left \
                     BYTE-IDENTICAL) — the nonce was not in the live registry, so an \
                     MCP client that cached it was 401ing with no in-band recovery"
                );
            }
            ReconcileAction::Rewrite => {
                if !coord_mcp_safe_to_write(&workdir, IntendedWrite::Device) {
                    // An agent-spawn static-bearer config (or a user's own
                    // file) — never clobber. (A proxy-shaped config is
                    // ours-refreshable, so this only skips configs we must not
                    // touch.)
                    continue;
                }
                write_coord_mcp_proxy_config(&workdir, bound_port);
                counts.rewritten += 1;
                info!("coord_mcp: reconciled {workdir}/.mcp.json to bound port :{bound_port}");
            }
            ReconcileAction::UpgradeHeaders => {
                if !coord_mcp_safe_to_write(&workdir, IntendedWrite::Device) {
                    continue;
                }
                // SAFETY: the resolver only yields UpgradeHeaders when a
                // non-empty nonce was read from disk, so this Option is always
                // Some here — the same invariant the root path relies on.
                let nonce = on_disk_nonce
                    .expect("UpgradeHeaders implies a readable on-disk nonce (resolver invariant)");
                // Counted only when the write ACTUALLY landed: `write_mcp_json`
                // warns and swallows a permission failure, and a boot line that
                // counted the attempt would report a repair that did not happen.
                if rewrite_config_preserving_nonce(&workdir, bound_port, &nonce) {
                    counts.upgraded += 1;
                    info!(
                        "coord_mcp: boot reconcile UPGRADED the header shape of session \
                         {workdir}/.mcp.json (port :{bound_port} and nonce BOTH unchanged) — \
                         the file carried only the legacy proxy-key header, which leaves the \
                         next client launched against it escalating a 401 into OAuth/DCR"
                    );
                }
            }
        }
    }
    if counts.rewritten > 0 || counts.upgraded > 0 || counts.adopted > 0 {
        info!(
            "coord_mcp: boot reconcile rewrote {} session config(s) to the current bound port, \
             upgraded {} to the non-escalating header shape (nonce preserved), and ADOPTED {} \
             unregistered on-disk nonce(s) (no file written)",
            counts.rewritten, counts.upgraded, counts.adopted
        );
    }
    counts
}

/// How deep under the workspace root [`census_on_disk_mcp_configs`] walks.
///
/// Four levels is what the plan's out-of-band measurement used, and it is the
/// depth that reaches the shapes that actually exist: `<root>/.mcp.json` (0),
/// `<root>/<repo>/.mcp.json` (1), and the allocated-worktree form
/// `<root>/<repo>/agent-worktrees/<uuid>/<repo>/.mcp.json` (4).
const CONFIG_CENSUS_MAX_DEPTH: usize = 4;

/// Directory names the census never descends into. Dependency and build trees
/// hold no session workdir, and `node_modules` alone would multiply the walk by
/// four orders of magnitude at depth 4.
///
/// **The pruned walk is not "milliseconds"** — an earlier version of this
/// comment claimed that and it was never true. Measured on the operator box
/// 2026-08-25 with this exact prune list: **7.45 s warm-cache over 83,826
/// directories**, plus a `.mcp.json` stat in every one of them on top of the
/// `read_dir`. Boot is the COLD-cache case by construction, so the real figure
/// is worse. Pruning is what keeps this seconds rather than minutes; it does
/// not make it free.
///
/// That measurement is why the caller starts this walk and then goes and does
/// the reconcile, awaiting the census only when it builds the summary line — a
/// repair that a 401ing client is waiting on must not queue behind an
/// observability walk. See `mcp_api::start_server`'s boot reconcile task.
const CONFIG_CENSUS_PRUNED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cargo",
    ".pytest_cache",
];

/// The three-way classification of every `.mcp.json` on disk under the workspace
/// root (plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs`,
/// Phase 1).
///
/// **`open_backed` is the only class the boot reconcile can act on.** The other
/// two are reported so an operator reading the boot line can see the honest size
/// of the on-disk population WITHOUT mistaking it for the fix's reach: measured
/// 2026-08-25 the split was 11 / 2 / 578, so a summary printing only `total`
/// would overstate what [`reconcile_session_configs`] touches by ~54x. That is
/// exactly the `silent-empty-is-unknown` failure in reverse — a large number
/// standing in for a small one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OnDiskConfigCensus {
    /// Every `.mcp.json` found under the workspace root within
    /// [`CONFIG_CENSUS_MAX_DEPTH`]. Counted by PATH, not parsed — the
    /// classification below is about which session owns the directory, not
    /// about the file's shape, and parsing ~600 files on the boot path to learn
    /// nothing the classification uses would be pure cost.
    pub(crate) total: usize,
    /// Directory is the working dir of an OPEN lifecycle record — the class the
    /// boot reconcile is actually fed.
    pub(crate) open_backed: usize,
    /// Directory is the working dir of a lifecycle record that is no longer
    /// open (a dead session's leftovers).
    pub(crate) dead_backed: usize,
    /// No lifecycle record names this directory at all.
    pub(crate) orphaned: usize,
}

/// Normalized comparison key for a workdir path: lowercased, `\` folded to `/`,
/// trailing separators trimmed. Lifecycle records store whatever string the
/// spawner supplied while the census produces paths built by
/// [`crate::workspace_paths::workspace_root`], so the two spellings of one
/// directory routinely differ in separator and case on Windows. Comparing them
/// raw would classify every open workdir as orphaned and make the census a
/// confident lie.
///
/// `pub(crate)` because the boot task in `mcp_api::start_server` DEDUPES the
/// lifecycle-record workdirs through this same key before counting them or
/// handing them to [`reconcile_session_configs`] — two open terminals sharing
/// one cwd is legitimate and nothing upstream dedupes, so without it the boot
/// line's `adopted` count double-counts one file. Sharing the key (rather than
/// deduping on the raw string) is what keeps that count comparable with
/// [`OnDiskConfigCensus::open_backed`], which is normalized the same way.
pub(crate) fn workdir_census_key(path: &str) -> String {
    path.replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Walk the workspace root for `.mcp.json` files and classify each against the
/// lifecycle records — see [`OnDiskConfigCensus`].
///
/// `open_workdirs` is the set the boot task hands
/// [`reconcile_session_configs`]; `all_workdirs` is every workdir named by ANY
/// lifecycle record, open or closed. Both are normalized here, so callers pass
/// the raw record strings.
///
/// Returns `None` when no workspace root resolves — a statement of UNKNOWN, not
/// of an empty disk, and the boot summary must say so rather than print zeros.
pub(crate) fn census_on_disk_mcp_configs<'a, O, A>(
    open_workdirs: O,
    all_workdirs: A,
) -> Option<OnDiskConfigCensus>
where
    O: IntoIterator<Item = &'a str>,
    A: IntoIterator<Item = &'a str>,
{
    let root = crate::workspace_paths::workspace_root()?;
    Some(census_on_disk_mcp_configs_at(
        &root,
        open_workdirs,
        all_workdirs,
    ))
}

/// Root-injected core of [`census_on_disk_mcp_configs`], so the walk and the
/// classification are unit-testable against a temp tree without touching the
/// operator's workspace or process-global env.
fn census_on_disk_mcp_configs_at<'a, O, A>(
    root: &Path,
    open_workdirs: O,
    all_workdirs: A,
) -> OnDiskConfigCensus
where
    O: IntoIterator<Item = &'a str>,
    A: IntoIterator<Item = &'a str>,
{
    let open: std::collections::HashSet<String> =
        open_workdirs.into_iter().map(workdir_census_key).collect();
    let known: std::collections::HashSet<String> =
        all_workdirs.into_iter().map(workdir_census_key).collect();

    let mut census = OnDiskConfigCensus::default();
    // Iterative walk with an explicit depth, rather than a recursive one: the
    // depth bound is the whole cost control here, and a bound that lives in the
    // loop cannot be lost to a refactor that inlines the recursion.
    let mut frontier: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = frontier.pop() {
        if dir.join(".mcp.json").is_file() {
            census.total += 1;
            let key = workdir_census_key(&dir.to_string_lossy());
            if open.contains(&key) {
                census.open_backed += 1;
            } else if known.contains(&key) {
                census.dead_backed += 1;
            } else {
                census.orphaned += 1;
            }
        }
        if depth >= CONFIG_CENSUS_MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            // An unreadable directory is skipped, not fatal: a census that
            // refuses to report because one subtree denied permission is worse
            // than one that reports a floor.
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if CONFIG_CENSUS_PRUNED_DIRS.contains(&name.as_str()) {
                continue;
            }
            frontier.push((path, depth + 1));
        }
    }
    census
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
    /// DEFAULT (no handshake presented) is denied. Pure resolver ⇒ no
    /// process-env, `OnceLock` or home-dir mutation.
    #[test]
    fn session_identity_gate_requires_handshake_and_marker_and_defaults_denied() {
        const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        // No handshake ⇒ NoHandshake, regardless of the marker. The caller must
        // not learn the machine's opt-in state before proving same-user.
        assert_eq!(
            resolve_session_identity_gate(None, Some(KEY), false),
            Err(SessionIdentityDenial::NoHandshake)
        );
        assert_eq!(
            resolve_session_identity_gate(None, Some(KEY), true),
            Err(SessionIdentityDenial::NoHandshake),
            "an opted-in machine must STILL be denied without the same-user handshake"
        );
        // An empty / whitespace-only header is "not presented", not "wrong".
        assert_eq!(
            resolve_session_identity_gate(Some(""), Some(KEY), true),
            Err(SessionIdentityDenial::NoHandshake)
        );
        assert_eq!(
            resolve_session_identity_gate(Some("   "), Some(KEY), true),
            Err(SessionIdentityDenial::NoHandshake)
        );

        // Presented but wrong ⇒ a DISTINCT reason (different fix).
        assert_eq!(
            resolve_session_identity_gate(Some("not-the-key"), Some(KEY), true),
            Err(SessionIdentityDenial::HandshakeMismatch)
        );
        // A PREFIX of the real key must not pass — the compare is whole-value.
        assert_eq!(
            resolve_session_identity_gate(Some(&KEY[..32]), Some(KEY), true),
            Err(SessionIdentityDenial::HandshakeMismatch)
        );
        // No key initialized on this runner ⇒ nothing can match: fail CLOSED,
        // never "anything goes".
        assert_eq!(
            resolve_session_identity_gate(Some(KEY), None, true),
            Err(SessionIdentityDenial::HandshakeMismatch)
        );
        assert_eq!(
            resolve_session_identity_gate(Some(KEY), Some(""), true),
            Err(SessionIdentityDenial::HandshakeMismatch)
        );

        // Handshake proven but not opted in ⇒ the third, distinct reason.
        assert_eq!(
            resolve_session_identity_gate(Some(KEY), Some(KEY), false),
            Err(SessionIdentityDenial::NotOptedIn)
        );
        // Both ⇒ allowed (surrounding whitespace on the header is tolerated).
        assert_eq!(
            resolve_session_identity_gate(Some(KEY), Some(KEY), true),
            Ok(())
        );
        assert_eq!(
            resolve_session_identity_gate(Some(&format!(" {KEY}\n")), Some(KEY), true),
            Ok(())
        );

        // The three denials never collapse into one code — a caller must be able
        // to tell "you did not prove same-user" from "you proved it wrong" from
        // "this machine has not opted in".
        let codes = [
            SessionIdentityDenial::NoHandshake.code(),
            SessionIdentityDenial::HandshakeMismatch.code(),
            SessionIdentityDenial::NotOptedIn.code(),
        ];
        for (i, a) in codes.iter().enumerate() {
            for b in codes.iter().skip(i + 1) {
                assert_ne!(a, b, "denial codes must stay distinct");
            }
        }
        assert_eq!(
            SessionIdentityDenial::NoHandshake.code(),
            "COORD_MCP_PROVISION_NO_HANDSHAKE"
        );
        assert_eq!(
            SessionIdentityDenial::HandshakeMismatch.code(),
            "COORD_MCP_PROVISION_HANDSHAKE_MISMATCH"
        );

        // The messages name the exact lever: the key file for the two handshake
        // denials, the marker file for the opt-in one.
        assert!(SessionIdentityDenial::NoHandshake
            .message()
            .contains(RUNNER_LOOPBACK_KEY_FILE));
        assert!(SessionIdentityDenial::NoHandshake
            .message()
            .contains(RUNNER_LOOPBACK_KEY_HEADER));
        assert!(SessionIdentityDenial::HandshakeMismatch
            .message()
            .contains(RUNNER_LOOPBACK_KEY_FILE));
        assert!(SessionIdentityDenial::NotOptedIn
            .message()
            .contains(SESSION_IDENTITY_MARKER_FILE));

        // No denial message may ever carry the secret itself.
        for d in [
            SessionIdentityDenial::NoHandshake,
            SessionIdentityDenial::HandshakeMismatch,
            SessionIdentityDenial::NotOptedIn,
        ] {
            assert!(
                !d.message().contains(KEY),
                "a denial must never echo the handshake secret"
            );
        }
    }

    /// The DELETED master env flag must not creep back as an override. The
    /// `FlagOff` variant is gone (a compile-time absence — this file would not
    /// build if a match arm still named it), and no code in this module reads
    /// the old env var name any more.
    #[test]
    fn the_master_env_flag_arm_is_deleted_not_deprecated() {
        // Setting the retired flag must not change any verdict: the resolver
        // has no env input at all, and the live gate is closed in this process
        // because no handshake key was ever initialized.
        assert_eq!(
            session_identity_gate(Some("anything")),
            Err(SessionIdentityDenial::HandshakeMismatch),
            "with no key initialized the live gate must fail CLOSED"
        );
        assert_eq!(
            session_identity_gate(None),
            Err(SessionIdentityDenial::NoHandshake)
        );
        // The retired name must appear in no CODE in this module.
        //
        // Deliberately NOT a raw `contains` over the whole file: the name
        // legitimately survives in PROSE (the module header records why it was
        // retired, which is worth keeping) and in this test's own needle, so a
        // whole-file scan asserts against its own source and can never pass. It
        // shipped that way and CI caught it. `concat!` keeps the needle itself
        // off the haystack; the comment filter handles the header.
        const RETIRED_FLAG: &str = concat!("QONTINUI_SESSION_COORD_", "IDENTITY_ENABLED");
        let src = include_str!("coord_mcp.rs");
        let offenders: Vec<String> = src
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim_start().starts_with("//"))
            .filter(|(_, l)| l.contains(RETIRED_FLAG))
            .map(|(i, l)| format!("line {}: {}", i + 1, l.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "the retired master flag must be DELETED, not left as an override; \
             still read by: {offenders:?}"
        );
    }

    /// Constant-time compare: correct verdicts (that is what a unit test can
    /// assert — timing is a property of `subtle`, which is what it is for).
    #[test]
    fn secret_eq_matches_only_the_whole_value() {
        assert!(secret_eq(b"abc123", b"abc123"));
        assert!(!secret_eq(b"abc123", b"abc124"));
        assert!(!secret_eq(b"abc123", b"abc12"));
        assert!(!secret_eq(b"abc12", b"abc123"));
        assert!(secret_eq(b"", b""));
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
        let pty_nonce = register_proxy_nonce(&wd, None);
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

        // THE LIVE-REVOCATION PROPERTY, asserted rather than assumed (plan
        // 2026-08-24 Phase 1). Opted IN, the freshly-minted ephemeral nonce
        // resolves…
        let marker = MarkerOverride::set(true);
        assert!(
            proxy_nonce_is_valid(&bare_nonce),
            "an ephemeral nonce must validate while the machine is opted in"
        );
        assert_eq!(
            proxy_principal_for_nonce(&bare_nonce),
            Some(ProxyPrincipal::Device),
            "the mint route issues a DEVICE nonce — never an agent one"
        );

        // …and DELETING the marker revokes it immediately — not merely blocking
        // future mints. That is the operator's real off switch, and it is what
        // makes the marker (rather than a spawn-time env flag) the second gate.
        marker.flip(false);
        assert!(
            !proxy_nonce_is_valid(&bare_nonce),
            "an ephemeral nonce must stop validating the moment the machine is opted out"
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
                    session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                    terminal_id: None,
                    minted_at: std::time::SystemTime::now(),
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
                    session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                    terminal_id: None,
                    minted_at: std::time::SystemTime::now(),
                },
            );
        }

        // Any mint triggers the opportunistic sweep.
        let persistent = register_proxy_nonce(&wd, None);

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
                session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                terminal_id: None,
                minted_at: std::time::SystemTime::now(),
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
        let persistent = register_proxy_nonce(&wd, None);
        assert!(proxy_nonce_is_valid(&persistent));
    }

    /// Stage 1 — a persistent mint FREEZES the terminal it was provisioned for,
    /// and [`terminal_id_for_nonce`] reads it back. This is the deterministic
    /// leg of caller self-identification; without it the resolver only has the
    /// 1:N workdir.
    ///
    /// (This assertion is not optional bookkeeping: `src-tauri/Cargo.toml` sets
    /// `[lints.rust] dead_code = "allow"`, so an UNREAD field produces no build
    /// warning in this crate. A test that reads it is the only deadness check
    /// available here.)
    #[test]
    fn terminal_id_for_nonce_returns_the_minted_terminal() {
        let wd = format!("D:/selfid-terminal-{}", uuid::Uuid::now_v7());
        let term = format!("terminal-{}", uuid::Uuid::now_v7());

        let nonce = register_proxy_nonce(&wd, Some(term.as_str()));

        assert_eq!(
            terminal_id_for_nonce(&nonce).as_deref(),
            Some(term.as_str()),
            "a persistent mint must freeze the terminal it was provisioned for"
        );
        // The workdir leg is unchanged and still resolves alongside it.
        assert_eq!(workdir_for_nonce(&nonce).as_deref(), Some(wd.as_str()));

        // Same `live_binding` chokepoint ⇒ same expiry/revocation rules: an
        // unknown nonce resolves to no terminal, never a stale one.
        assert_eq!(terminal_id_for_nonce("no-such-nonce"), None);
        assert_eq!(terminal_id_for_nonce(""), None);
    }

    /// Stage 1, THE point of the change — two terminals sharing ONE workdir get
    /// two DISTINCT live nonces, each resolving to its own terminal. The 1:1
    /// property caller self-identification needs.
    ///
    /// It also pins the eviction narrowing that makes it possible: the second
    /// terminal's mint must NOT evict the first's nonce. Under the old
    /// workdir-only rule it would have, 401ing terminal 1's already-connected
    /// MCP client the moment terminal 2 spawned in the same repo dir — the
    /// sibling-DoS the ephemeral class already had to fix.
    #[test]
    fn two_terminals_in_one_workdir_get_two_nonces_each_naming_its_own_terminal() {
        let wd = format!("D:/selfid-two-terminals-{}", uuid::Uuid::now_v7());
        let t1 = format!("terminal-a-{}", uuid::Uuid::now_v7());
        let t2 = format!("terminal-b-{}", uuid::Uuid::now_v7());

        let n1 = register_proxy_nonce(&wd, Some(t1.as_str()));
        let n2 = register_proxy_nonce(&wd, Some(t2.as_str()));
        assert_ne!(n1, n2, "each terminal gets its own nonce");

        assert!(
            proxy_nonces().lock().unwrap().contains_key(&n1),
            "a second terminal's mint must NOT evict the first terminal's LIVE \
             nonce for the same workdir (it would 401 its MCP client mid-session)"
        );
        assert!(proxy_nonce_is_valid(&n1));
        assert!(proxy_nonce_is_valid(&n2));

        assert_eq!(terminal_id_for_nonce(&n1).as_deref(), Some(t1.as_str()));
        assert_eq!(terminal_id_for_nonce(&n2).as_deref(), Some(t2.as_str()));
        // Both still name the same workdir — which is exactly why the workdir
        // leg alone can never disambiguate them.
        assert_eq!(workdir_for_nonce(&n1).as_deref(), Some(wd.as_str()));
        assert_eq!(workdir_for_nonce(&n2).as_deref(), Some(wd.as_str()));

        // Re-provisioning the SAME terminal (a re-spawn into the same cwd) still
        // evicts its own predecessor — narrowed, not removed.
        let n1b = register_proxy_nonce(&wd, Some(t1.as_str()));
        assert_ne!(n1b, n1);
        assert!(
            !proxy_nonces().lock().unwrap().contains_key(&n1),
            "a same-terminal re-mint still evicts that terminal's prior nonce"
        );
        assert!(
            proxy_nonces().lock().unwrap().contains_key(&n2),
            "...and never touches the sibling terminal's"
        );
    }

    /// Regression guard for the UNCHANGED path: a terminal-less mint resolves to
    /// `terminal_id: None` (so the resolver falls back to the workdir leg rather
    /// than inventing an identity), and it still evicts a prior terminal-less
    /// nonce for the same workdir — byte-for-byte the pre-Stage-1 rule.
    ///
    /// It must also leave a per-TERMINAL nonce for that workdir alone: the
    /// in-cwd `.mcp.json` writer and the boot self-heal both mint terminal-less,
    /// and neither may 401 a live terminal's client.
    #[test]
    fn terminalless_mint_has_no_terminal_and_still_evicts_its_own_class() {
        let wd = format!("D:/selfid-terminalless-{}", uuid::Uuid::now_v7());
        let term = format!("terminal-live-{}", uuid::Uuid::now_v7());

        let owned = register_proxy_nonce(&wd, Some(term.as_str()));
        let a = register_proxy_nonce(&wd, None);
        assert_eq!(
            terminal_id_for_nonce(&a),
            None,
            "a terminal-less mint must not claim a terminal — a wrong identity \
             is worse than an absent one"
        );
        assert_eq!(workdir_for_nonce(&a).as_deref(), Some(wd.as_str()));

        let b = register_proxy_nonce(&wd, None);
        assert!(
            !proxy_nonces().lock().unwrap().contains_key(&a),
            "a terminal-less re-provision into the same cwd still evicts its \
             terminal-less predecessor (unchanged behavior)"
        );
        assert!(proxy_nonces().lock().unwrap().contains_key(&b));
        assert!(
            proxy_nonces().lock().unwrap().contains_key(&owned),
            "a terminal-less mint must never evict a per-terminal nonce for the \
             same workdir"
        );
        assert_eq!(
            terminal_id_for_nonce(&owned).as_deref(),
            Some(term.as_str())
        );
    }

    /// §1/E — an ephemeral nonce NEVER reaches disk. The store has no expiry
    /// column, so a persisted ephemeral nonce would restore as an UNBOUNDED
    /// one — laundering the weaker class into the stronger one across a restart.
    /// The runner-spawn nonce in the same snapshot still persists.
    #[test]
    fn ephemeral_nonces_are_never_persisted() {
        let (dir, store) = temp_store("ephemeral-never-persisted");
        let wd = format!("D:/persist-test/{}", uuid::Uuid::now_v7());

        let persistent = register_proxy_nonce(&wd, None);
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
        // Phase 2 (plan 2026-08-20): the nonce ALSO travels in `Authorization`,
        // because a static `Authorization` in the headers map is what stops the
        // MCP client attaching an OAuth provider — and therefore what stops a
        // stale key's 401 escalating into DCR. The invariant this assertion has
        // always been protecting is unchanged and now stated exactly: the value
        // is the NONCE, never a baked JWT.
        assert_eq!(server["headers"]["Authorization"], "Bearer abc123");
        assert!(
            !crate::coord_mcp_config::looks_like_jwt("abc123"),
            "the proxy shape must never bake a static bearer TOKEN"
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
            provision_coord_mcp_config_file(&wd, None).is_none(),
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

    /// The `--mcp-config` filename is stable for a given key and distinct across
    /// keys, on BOTH axes: it varies with the TERMINAL (Stage 1 — this is what
    /// stops two terminals in one cwd from sharing one file and therefore one
    /// nonce) **and** with the WORKDIR, because the key is the
    /// `(workdir, terminal_id)` PAIR. Terminal-less callers hash the workdir
    /// alone, unchanged.
    ///
    /// The same-terminal-different-cwd assertion is INVERTED from what it was.
    /// It used to pin "the terminal id is the key when present", i.e. `(W1, T)`
    /// and `(W2, T)` share one filename. That disagreed with the nonce
    /// registry, which evicts on the `(workdir, terminal_id)` PAIR: a terminal
    /// re-provisioned into a new cwd overwrote the one file with the new nonce
    /// while the `(W1, T)` binding — never matched by the eviction rule, and
    /// with no TTL to expire it — stayed live and valid with no file pointing
    /// at it. A superseded credential left accepting requests. Two keys, two
    /// files: eviction and rewrite now cover exactly the same set.
    #[test]
    fn mcp_config_file_name_is_stable_and_workdir_distinct() {
        let a1 = mcp_config_file_name("D:/repo/one", None);
        let a2 = mcp_config_file_name("D:/repo/one", None);
        let b = mcp_config_file_name("D:/repo/two", None);
        assert_eq!(a1, a2, "stable across calls for one workdir");
        assert_ne!(a1, b, "distinct across workdirs");
        assert!(a1.starts_with("coord-mcp-") && a1.ends_with(".json"));

        // THE Stage-1 property: two terminals sharing ONE cwd get two names.
        let t1 = mcp_config_file_name("D:/repo/one", Some("term-1"));
        let t2 = mcp_config_file_name("D:/repo/one", Some("term-2"));
        assert_ne!(
            t1, t2,
            "two terminals in one workdir must get distinct --mcp-config files \
             (one shared file ⇒ one shared nonce ⇒ caller self-id cannot resolve)"
        );
        assert_eq!(
            t1,
            mcp_config_file_name("D:/repo/one", Some("term-1")),
            "stable per terminal across re-spawns"
        );
        assert!(t1.starts_with("coord-mcp-") && t1.ends_with(".json"));

        // THE eviction-agreement property: the key is the PAIR, so the same
        // terminal re-provisioned into a DIFFERENT cwd gets a DIFFERENT name.
        // Sharing one name there orphaned the old `(workdir, terminal)`
        // binding — never evicted, no TTL, still valid.
        assert_ne!(
            t1,
            mcp_config_file_name("D:/repo/elsewhere", Some("term-1")),
            "same terminal in a different cwd must get a different file: the \
             nonce registry evicts on (workdir, terminal_id), so a shared name \
             would leave the prior binding live-but-unreachable"
        );
        // The terminal-less shape still hashes the workdir alone, and does not
        // collide with the paired names.
        assert_ne!(t1, a1);
        assert_ne!(t2, a1);
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

    /// Serializes every test that calls [`restore_proxy_nonces_from`].
    ///
    /// The `restore` forensics line is an AGGREGATE — no workdir, no key prefix
    /// — so unlike every other line in this stream it cannot be filtered to the
    /// test that produced it. Tests run concurrently in one process against one
    /// shared log ([`rotation_log_test_dir`]), so without this a peer's restore
    /// line lands inside another test's read window. Take it in ANY test that
    /// triggers a restore, not only in the ones that read the log back.
    static RESTORE_FORENSICS_LOCK: Mutex<()> = Mutex::new(());

    fn restore_forensics_lock() -> std::sync::MutexGuard<'static, ()> {
        // A panicking peer test must not cascade into unrelated failures here:
        // the guard protects an ordering, not an invariant, so poison is
        // recovered rather than propagated.
        RESTORE_FORENSICS_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Build one persisted-store value in whatever shape the store currently
    /// takes, so the store-shape tests state their intent (workdir + terminal +
    /// age) once rather than tracking the schema at every call site.
    ///
    /// `minted_at_unix: None` is the LEGACY age — an entry written before the
    /// field existed. It is not a neutral default: the restore leg orders it as
    /// oldest.
    fn stored_binding(
        workdir: &str,
        terminal_id: Option<&str>,
        minted_at_unix: Option<u64>,
    ) -> crate::secure_storage::StoredNonceBinding {
        crate::secure_storage::StoredNonceBinding {
            workdir: workdir.to_string(),
            terminal_id: terminal_id.map(str::to_string),
            minted_at_unix,
        }
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

        // NO baked bearer TOKEN — the whole point is the proxy injects a live
        // one per request. Since Phase 2 the `Authorization` header IS present,
        // carrying the same nonce (the OAuth-provider suppressor); what must
        // never appear there is a JWT.
        assert_eq!(
            server["headers"]["Authorization"],
            serde_json::Value::from(format!("Bearer {nonce}")),
            "proxy shape must carry the nonce as a bearer: {written}"
        );
        assert!(
            !crate::coord_mcp_config::looks_like_jwt(nonce),
            "proxy shape must NOT bake a static Authorization TOKEN: {written}"
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
        let nonce = register_proxy_nonce(&dir.to_string_lossy(), None);
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
        let dev_nonce = register_proxy_nonce(&dev_dir.to_string_lossy(), None);
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
        let nonce = register_proxy_nonce(&dir.to_string_lossy(), None);
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
            ..Default::default()
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
        assert_eq!(
            server["headers"]["Authorization"],
            serde_json::Value::from(format!("Bearer {nonce}")),
            "agent proxy shape carries the nonce as a bearer too (Phase 2): {written}"
        );
        assert!(
            !crate::coord_mcp_config::looks_like_jwt(nonce),
            "agent proxy shape must NOT bake a static bearer TOKEN: {written}"
        );
        // The nonce is bound to THIS agent.
        assert_eq!(
            proxy_principal_for_nonce(nonce),
            Some(ProxyPrincipal::Agent { agent_id })
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Layer 14's shape classifier, arm by arm, against LITERAL documents.
    ///
    /// The classification is the part a report can get wrong — it is what
    /// decides whether an operator reads "your config is fine" or "something
    /// else owns this file" — so every arm is pinned against a document written
    /// out here rather than against a real umbrella root, which would make the
    /// verdict depend on the machine the test runs on.
    #[test]
    fn classify_mcp_json_doc_separates_ours_foreign_and_unparseable() {
        use serde_json::json;

        // Our loopback PROXY shape.
        assert_eq!(
            classify_mcp_json_doc(&json!({
                "mcpServers": {"coord-mcp": {"url": "http://127.0.0.1:9876/coord-mcp"}}
            })),
            McpJsonShape::OursProxy
        );
        // Ours, but the static-bearer (agent JWT) shape — no proxy URL.
        assert_eq!(
            classify_mcp_json_doc(&json!({
                "mcpServers": {"coord-mcp": {"url": "https://coord.qontinui.io/mcp"}}
            })),
            McpJsonShape::OursStaticBearer
        );
        // A second server means the operator owns this file.
        assert_eq!(
            classify_mcp_json_doc(&json!({
                "mcpServers": {
                    "coord-mcp": {"url": "http://127.0.0.1:9876/coord-mcp"},
                    "some-other": {"command": "node"}
                }
            })),
            McpJsonShape::Foreign
        );
        // One server that is not ours at all.
        assert_eq!(
            classify_mcp_json_doc(&json!({"mcpServers": {"some-other": {"command": "node"}}})),
            McpJsonShape::Foreign
        );
        // Valid JSON with no `mcpServers` object.
        assert_eq!(
            classify_mcp_json_doc(&json!({"hello": "world"})),
            McpJsonShape::Unparseable
        );

        // The wire strings are a contract a reader compares across machines —
        // literals, not `as_str()` round-trips.
        assert_eq!(McpJsonShape::OursProxy.as_str(), "ours_proxy");
        assert_eq!(
            McpJsonShape::OursStaticBearer.as_str(),
            "ours_static_bearer"
        );
        assert_eq!(McpJsonShape::Foreign.as_str(), "foreign");
        assert_eq!(McpJsonShape::Absent.as_str(), "absent");
        assert_eq!(McpJsonShape::NoRoot.as_str(), "no_workspace_root");
        assert_eq!(
            McpJsonShape::Unparseable.as_str(),
            "unparseable_or_no_mcp_servers"
        );
    }

    /// The report NEVER carries the bearer or the proxy nonce.
    ///
    /// **F7 regression.** `exists: true; shape: absent` was a state the report
    /// could print, and it is self-contradictory: only a `NotFound` read is an
    /// ABSENT file. A permission denial, an exclusive lock or non-UTF-8 bytes
    /// are a file that IS there and cannot be read — `unparseable` — and the
    /// reason must survive, because "go find the missing file" and "go find
    /// what is holding the lock" are different jobs.
    ///
    /// Asserted against LITERAL wire strings, not `as_str()` round-trips.
    ///
    /// **The F7 fix reopened with the polarity flipped, and this is the
    /// regression for the second one.** `exists` was still taken from a separate
    /// `path.is_file()` while `shape` came from `shape_from_read`, so a
    /// `.mcp.json` that is a DIRECTORY rendered `on disk: false` next to
    /// `The file IS on disk and could not be read` — the same self-contradiction,
    /// inverted. `exists` now comes out of the SAME `io::Result`, so every case
    /// below asserts both, and the two are incapable of disagreeing.
    #[test]
    fn mcp_json_read_error_is_unparseable_not_absent_unless_it_is_notfound() {
        use std::io::{Error, ErrorKind};

        // The ONLY arm that may be `absent` — and the only one that is not on
        // disk.
        let (exists, shape, reason) = shape_from_read(Err(Error::new(ErrorKind::NotFound, "nope")));
        assert_eq!(shape.as_str(), "absent");
        assert!(!exists, "a NotFound read is the one absent case");
        assert_eq!(reason, None, "an absent file has no read failure to report");

        // Everything else is a PRESENT file that could not be read.
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData, // non-UTF-8 bytes
            ErrorKind::Other,       // Windows sharing violation surfaces here
            // A DIRECTORY at `.mcp.json`: `is_file()` says false, the read
            // fails non-NotFound. This is the exact pair that contradicted.
            ErrorKind::IsADirectory,
        ] {
            let (exists, shape, reason) =
                shape_from_read(Err(Error::new(kind, "held by another process")));
            assert_eq!(
                shape.as_str(),
                "unparseable_or_no_mcp_servers",
                "{kind:?} names a file that exists and cannot be read"
            );
            assert!(
                exists,
                "{kind:?}: `unparseable` means present-and-unusable, so `exists` must agree"
            );
            assert_eq!(
                reason.as_deref(),
                Some("held by another process"),
                "{kind:?}: the reason is the whole difference from `absent`"
            );
        }

        // A syntactically broken document is `unparseable` too, and its reason
        // is BOUNDED — this file carries a bearer and a proxy nonce, and
        // `serde_json`'s Display quotes the offending token out of the source.
        let bearer = "eyJhbGciOiJFZERTQSJ9.cGF5bG9hZA.c2ln";
        let (exists, shape, reason) = shape_from_read(Ok(format!(
            "{{\"mcpServers\": {{\"coord-mcp\": {{\"headers\": {{\"Authorization\": \"Bearer {bearer}\"}}"
        )));
        assert_eq!(shape.as_str(), "unparseable_or_no_mcp_servers");
        assert!(
            exists,
            "a document that parsed badly was still read off disk"
        );
        let reason = reason.expect("a parse failure records its reason");
        assert!(
            !reason.contains(bearer),
            "the parse reason leaked the bearer: {reason}"
        );
        assert!(
            reason.starts_with("JSON ") && reason.contains(" error at line "),
            "the reason must be category + position: {reason}"
        );

        // A well-formed document still classifies normally, with no reason.
        let (exists, shape, reason) = shape_from_read(Ok(
            "{\"mcpServers\": {\"coord-mcp\": {\"url\": \"http://127.0.0.1:9876/coord-mcp\"}}}"
                .to_string(),
        ));
        assert_eq!(shape.as_str(), "ours_proxy");
        assert!(exists);
        assert_eq!(reason, None);

        // THE CONTRADICTION CONTROL, stated as an invariant over every arm:
        // `absent` iff not on disk. A row can no longer say "on disk: false"
        // beside "The file IS on disk", in either direction.
        for read in [
            Err(Error::new(ErrorKind::NotFound, "x")),
            Err(Error::new(ErrorKind::IsADirectory, "x")),
            Err(Error::new(ErrorKind::PermissionDenied, "x")),
            Ok("{}".to_string()),
            Ok("{\"mcpServers\":{\"other\":{}}}".to_string()),
        ] {
            let (exists, shape, _) = shape_from_read(read);
            assert_eq!(
                exists,
                shape != McpJsonShape::Absent,
                "`exists` and `absent` must be exact complements, got exists={exists} shape={}",
                shape.as_str()
            );
        }
    }

    /// **A real directory at `<root>/.mcp.json`**, driven through the filesystem
    /// rather than a synthesized `io::Result` — because the whole point of the
    /// defect was that the OS answers `is_file()` and `read_to_string()`
    /// differently for this one shape, and a hand-built `Err` cannot prove the
    /// platform actually does that.
    #[test]
    fn mcp_json_shape_for_a_directory_at_the_path_agrees_with_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(".mcp.json");
        std::fs::create_dir(&path).expect("mkdir .mcp.json");

        // The pair the old code combined: they disagree, which is why deriving
        // BOTH from one observation is the fix and not a style preference.
        assert!(!path.is_file(), "a directory is not a file");
        let read = std::fs::read_to_string(&path);
        assert!(read.is_err(), "reading a directory fails");
        assert_ne!(
            read.as_ref().unwrap_err().kind(),
            std::io::ErrorKind::NotFound,
            "…and it fails as something other than NotFound"
        );

        let (exists, shape, reason) = shape_from_read(read);
        assert!(exists, "the path IS on disk");
        assert_eq!(shape.as_str(), "unparseable_or_no_mcp_servers");
        assert!(reason.is_some(), "present-and-unusable carries its reason");
    }

    /// Structural, not textual: [`McpJsonReport`] has no field able to hold
    /// either, so this asserts against a document stuffed with both and checks
    /// the whole `Debug` rendering — the widest surface the struct can leak
    /// through, and the one a `serde_json` or a panic message would use.
    #[test]
    fn mcp_json_report_shape_cannot_carry_the_bearer_or_the_nonce() {
        use serde_json::json;

        let nonce = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let bearer = "eyJhbGciOiJFZERTQSJ9.PAYLOAD.SIGNATURE";
        let doc = json!({
            "mcpServers": {"coord-mcp": {
                "url": "http://127.0.0.1:9876/coord-mcp",
                "headers": {"Authorization": format!("Bearer {bearer}"),
                            "X-Coord-Mcp-Proxy-Key": nonce}
            }}
        });
        assert_eq!(classify_mcp_json_doc(&doc), McpJsonShape::OursProxy);

        let report = McpJsonReport {
            root: Some("D:/qontinui-root".to_string()),
            path: Some("D:/qontinui-root/.mcp.json".to_string()),
            exists: true,
            instance_name: None,
            owns_shared_root_state: true,
            this_runner_port: Some(9876),
            proxy_port: Some(9876),
            shape: classify_mcp_json_doc(&doc),
            read_error: None,
            safe_to_write: true,
        };
        let rendered = format!("{report:?}");
        assert!(
            !rendered.contains(nonce),
            "the proxy nonce leaked: {rendered}"
        );
        assert!(!rendered.contains(bearer), "the bearer leaked: {rendered}");
    }

    /// **The split introduced for the config report is not a divergence.**
    ///
    /// Layer 14 stopped asking [`coord_mcp_safe_to_write`] and started asking
    /// [`coord_mcp_write_verdict`], so that opening the report on a secondary no
    /// longer emits `coord_mcp: REFUSING to write …` into the runner log the
    /// operator is about to read — a log line describing a write nobody
    /// attempted. The hazard that trade introduces is the one this whole report
    /// exists to expose: two doors onto one rule, free to drift apart.
    ///
    /// They cannot, because the wrapper IS the core plus a log line — and this
    /// asserts it over every shape the guard classifies, including the two that
    /// hinge on JWT `sub_type` and the proxy shape whose 64-hex nonce must fail
    /// the JWT decode.
    #[test]
    fn the_report_verdict_core_agrees_with_the_warning_wrapper() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        let dir = std::env::temp_dir().join(format!("coord-mcp-split-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        let agent_jwt = format!("h.{}.s", URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#));
        // Built rather than written whole: a contiguous 64-hex literal beside the
        // `X-Coord-Mcp-Proxy-Key` keyword is exactly what gitleaks' `generic-api-key`
        // rule matches. The value is unchanged.
        let proxy_nonce = "0123456789abcdef".repeat(4);
        let device_jwt = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#)
        );

        // (absent) plus every on-disk shape, and the expected verdict for each
        // — LITERAL, so the split cannot be "proved" by comparing the two doors
        // to each other while both are wrong.
        let mut cases: Vec<(Option<String>, McpWriteVerdict, bool)> =
            vec![(None, McpWriteVerdict::Allowed, true)];
        for (doc, verdict, allowed) in [
            (
                r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"https://c/mcp"}}}"#.to_string(),
                McpWriteVerdict::Allowed,
                true,
            ),
            (
                r#"{"mcpServers":{"my-server":{"type":"http","url":"https://x/mcp"}}}"#.to_string(),
                McpWriteVerdict::RefusedExistingConfig,
                false,
            ),
            (
                r#"{"mcpServers":{"coord-mcp":{"url":"https://c/mcp"},"other":{"url":"x"}}}"#
                    .to_string(),
                McpWriteVerdict::RefusedExistingConfig,
                false,
            ),
            (
                "not json {{{".to_string(),
                McpWriteVerdict::RefusedExistingConfig,
                false,
            ),
            (
                // The static-bearer AGENT shape. It now classifies under the
                // MORE SPECIFIC `RefusedAgentPrincipal` arm rather than the
                // generic existing-config one: both refuse a device write, but
                // only this one is a refused scope elevation, and only this one
                // warns. Same `false`, different — and more honest — reason.
                format!(
                    r#"{{"mcpServers":{{"coord-mcp":{{"url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
                ),
                McpWriteVerdict::RefusedAgentPrincipal,
                false,
            ),
            (
                format!(
                    r#"{{"mcpServers":{{"coord-mcp":{{"url":"https://c/mcp","headers":{{"Authorization":"Bearer {device_jwt}"}}}}}}}}"#
                ),
                McpWriteVerdict::Allowed,
                true,
            ),
            (
                format!(
                    r#"{{"mcpServers":{{"coord-mcp":{{"url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{proxy_nonce}"}}}}}}}}"#
                ),
                McpWriteVerdict::Allowed,
                true,
            ),
        ] {
            cases.push((Some(doc), verdict, allowed));
        }

        for (i, (doc, expected_verdict, expected_bool)) in cases.into_iter().enumerate() {
            match &doc {
                Some(d) => std::fs::write(&mcp, d).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&mcp);
                }
            }
            // `wd` is a temp dir, never the umbrella root, so the shared-root arm
            // is out of the picture and this compares the existing-config half.
            let verdict = coord_mcp_write_verdict(&wd, IntendedWrite::Device);
            assert_eq!(verdict, expected_verdict, "case {i}: doc={doc:?}");
            assert_eq!(
                coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
                expected_bool,
                "case {i}: the warn!-emitting wrapper disagreed with its own core"
            );
            assert_eq!(
                verdict == McpWriteVerdict::Allowed,
                coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
                "case {i}: the two doors must be the same rule"
            );
        }

        // The shared-root arm, which the loop above cannot reach: this test
        // process owns shared root state (no `QONTINUI_INSTANCE_NAME`), so the
        // guard's first branch never fires for it. Asserted at the pure core
        // both doors call — that arm is the ONE that carries a `warn!`, which is
        // exactly why the report must not take the wrapper.
        assert!(
            !shared_root_write_allowed_at(&wd, Some(&dir), false),
            "a runner that does NOT own shared root state is refused AT the root"
        );
        assert!(
            shared_root_write_allowed_at(&wd, Some(&dir), true),
            "…and the owner is not — or the negative above is vacuous"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "absent file must be writable"
        );

        // 2. Solely our coord-mcp config → safe to refresh (we own it).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"https://c/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "a solely-coord-mcp config is ours — refreshable"
        );

        // 3. A user's own config (different server) → must NOT clobber.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"my-server":{"type":"http","url":"https://x/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "a foreign mcpServers config must be left untouched"
        );

        // 4. coord-mcp ALONGSIDE another server (2 keys) → not solely ours → skip.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"url":"https://c/mcp"},"other":{"url":"x"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "coord-mcp plus a user server is the user's file — do not clobber"
        );

        // 5. Unparseable / non-JSON → conservatively do not clobber.
        std::fs::write(&mcp, "not json {{{").unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
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
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
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
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
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
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "a proxy-shaped sole-coord-mcp config is ours — refreshable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The AGENT PROXY shape at the write chokepoint — the half of the
    /// principal-class guard that lives outside the boot resolvers.
    ///
    /// `write_coord_mcp_agent_proxy_config` emits the device document plus the
    /// principal marker, so the JWT decode in
    /// `existing_config_write_verdict` cannot see it: the `Authorization` value
    /// is a 64-hex nonce, which is never JWT-shaped by construction, so
    /// `jwt_unverified_claim` yields `None` and the file used to classify as an
    /// ordinary device config the device path may refresh.
    ///
    /// That mattered in production, not only in theory:
    /// `agent_runtime::run_continuation_terminal` / `run_continuation_headless`
    /// call `provision_coord_mcp_for_session` with the runner's DEVICE JWT for a
    /// workdir an agent spawn may already have written a marked config into, and
    /// neither boot resolver is anywhere in that path.
    #[test]
    fn device_writes_are_refused_over_an_agent_marked_proxy_config() {
        let dir = std::env::temp_dir().join(format!("qr-agentmark-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        // Exactly what the agent emitter writes, marker and all.
        let agent_id = Uuid::new_v4();
        write_coord_mcp_agent_proxy_config(&wd, 9876, agent_id);
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
        assert!(
            config_doc_is_agent_marked(&doc),
            "precondition: the agent emitter stamps the principal marker"
        );
        assert!(
            proxy_nonce_from_config_doc(&doc).is_some(),
            "precondition: it is the PROXY shape, so the device path would otherwise own it"
        );

        // The defect this pins: a DEVICE write must be refused, and the verdict
        // must be the distinct agent-principal arm rather than the generic
        // foreign-config one (the generic arm is silent by design; this one warns).
        assert!(
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "a device write over an AGENT-marked proxy config is a scope elevation"
        );
        assert_eq!(
            existing_config_write_verdict(&wd, IntendedWrite::Device),
            ExistingConfigVerdict::AgentPrincipal,
        );

        // ...and the AGENT path may still refresh its own config. The old guard
        // asked one question for both callers, which made the no-downgrade rule
        // non-directional despite its own doc saying "downgrade".
        assert!(
            coord_mcp_safe_to_write(&wd, IntendedWrite::Agent),
            "an agent write over an agent config is that writer's own file"
        );

        // The UNMARKED device proxy shape is untouched by this change — nothing
        // already on disk changes class, which is why the device shape omits the
        // header rather than spelling `device`.
        write_coord_mcp_proxy_config(&wd, 9876);
        assert!(
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "an unmarked device proxy config stays refreshable"
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
            server["headers"]["Authorization"]
                .as_str()
                .and_then(crate::coord_mcp_config::proxy_nonce_from_authorization)
                .is_some_and(proxy_nonce_is_valid),
            "device path must carry the registered NONCE as its bearer (Phase 2),              never a baked static token — the proxy injects the real one live"
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
            server["headers"]["Authorization"]
                .as_str()
                .and_then(crate::coord_mcp_config::proxy_nonce_from_authorization)
                .is_some(),
            "agent path must carry the NONCE as its bearer (Phase 2), never a              baked static token — the proxy injects the agent's live one"
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

    /// A bearer whose `sub_type` is neither `device` nor `agent` — the 2026-08-05
    /// tenant `SubType::Service` case — must leave a session-readable breadcrumb
    /// NAMING the observed `sub_type`, not just a `tracing::info!` the session
    /// cannot see. Without it the session discovers it has no coord identity only
    /// when a claim-gated tool refuses, mid-task.
    #[test]
    fn non_device_agent_bearer_writes_a_degraded_breadcrumb_naming_the_sub_type() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let svc = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"service"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-svc-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        provision_coord_mcp_with_jwt(&wd, &svc, Some(19876));

        assert!(
            !d.join(".mcp.json").exists(),
            "a Service bearer must still NOT be written (would 401 coord's verifier)"
        );
        let crumb = std::fs::read_to_string(d.join(COORD_MCP_STATUS_FILE))
            .expect("the sub_type skip must leave a session-readable breadcrumb");
        assert!(
            crumb.contains("coord-mcp UNREACHABLE") && crumb.contains("/gate"),
            "breadcrumb must be the actionable degraded line: {crumb}"
        );
        assert!(
            crumb.contains("sub_type=service"),
            "breadcrumb must NAME the observed sub_type so the session self-diagnoses: {crumb}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// The shared-root / foreign-config inheritance case: the workdir already
    /// holds someone else's `.mcp.json` (which is how the wrong bearer arrived on
    /// 2026-08-05). We correctly leave it alone — and must now say so where the
    /// session can read it.
    #[test]
    fn foreign_mcp_json_workdir_writes_a_degraded_breadcrumb() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-foreign-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();
        // A user's own config with NO coord-mcp entry → not ours to rewrite.
        let foreign = r#"{"mcpServers":{"some-other":{"type":"http","url":"https://x/mcp"}}}"#;
        std::fs::write(d.join(".mcp.json"), foreign).unwrap();

        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));

        assert_eq!(
            std::fs::read_to_string(d.join(".mcp.json")).unwrap(),
            foreign,
            "the user's own config must be preserved verbatim"
        );
        let crumb = std::fs::read_to_string(d.join(COORD_MCP_STATUS_FILE))
            .expect("the non-clobber skip must leave a session-readable breadcrumb");
        assert!(
            crumb.contains("coord-mcp UNREACHABLE") && crumb.contains("/gate"),
            "breadcrumb must be the actionable degraded line: {crumb}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// ...but NOT a false alarm. The no-downgrade guard also refuses a device
    /// write into a workdir that already holds a working AGENT coord-mcp config.
    /// That session's coord-mcp is fine — and nothing would ever clear a
    /// breadcrumb dropped here (no probe runs on this path), so it would be a
    /// permanent lie. Assert the breadcrumb is withheld.
    #[test]
    fn existing_agent_coord_mcp_config_gets_no_false_degraded_breadcrumb() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-nodown-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {}"}}}}}}}}"#,
            mk("agent")
        );
        std::fs::write(d.join(".mcp.json"), &agent_cfg).unwrap();

        provision_coord_mcp_with_jwt(&wd, &mk("device"), Some(19876));

        assert_eq!(
            std::fs::read_to_string(d.join(".mcp.json")).unwrap(),
            agent_cfg,
            "a device bearer must not downgrade an existing agent-JWT config"
        );
        assert!(
            !d.join(COORD_MCP_STATUS_FILE).exists(),
            "a workdir that already declares coord-mcp must NOT get an UNREACHABLE breadcrumb"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Plan 2026-09-02-coord-access-dies-by-eviction-not-expiry Phase F4 §2/§4:
    /// two sessions provisioned into ONE shared cwd through the in-cwd
    /// chokepoint — the gate-continuation canonical-checkout fallback and the
    /// boot-restore burst both take this exact path — must NOT evict each
    /// other. The second provision finds a live nonce on disk and reuses it:
    /// the file is byte-identical, the first nonce is still LIVE (not merely
    /// graced), exactly one persistent device binding exists for the cwd, and
    /// the forensics stream shows one `mint`, one `reuse` and zero `evict`
    /// lines. Before F4 the second call minted and the first session's MCP
    /// client was 401'd at the end of the grace window.
    #[test]
    fn in_cwd_reprovision_reuses_the_live_nonce_and_evicts_no_sibling() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let log_dir = rotation_log_test_dir();
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-f4-reuse-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        // Session 1 spawns into the shared cwd: no config yet → mint.
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let first = std::fs::read_to_string(d.join(".mcp.json")).unwrap();
        let n1 = read_proxy_nonce(&d.join(".mcp.json")).expect("first provision mints a nonce");
        assert!(live_binding(&n1).is_some());

        // Session 2 spawns into the SAME cwd while session 1 is live → reuse.
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let second = std::fs::read_to_string(d.join(".mcp.json")).unwrap();
        assert_eq!(
            second, first,
            "a reuse must leave a healthy config byte-identical"
        );
        assert!(
            live_binding(&n1).is_some(),
            "the sibling's nonce must still be LIVE — not evicted, not graced"
        );
        assert!(
            !graced_nonces().lock().unwrap().contains_key(&n1),
            "a reuse must never move the incumbent onto the grace TTL"
        );
        let persistent_device_for_wd = proxy_nonces()
            .lock()
            .unwrap()
            .values()
            .filter(|b| {
                b.workdir == wd
                    && b.principal == ProxyPrincipal::Device
                    && !b.lifetime.is_ephemeral()
            })
            .count();
        assert_eq!(
            persistent_device_for_wd, 1,
            "reuse registers nothing: still exactly one live binding for the cwd"
        );

        let raw = std::fs::read_to_string(log_dir.join(ROTATION_LOG_FILE)).unwrap();
        let mine: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["workdir"] == wd.as_str())
            .collect();
        let count = |event: &str| mine.iter().filter(|v| v["event"] == event).count();
        assert_eq!(count("mint"), 1, "exactly one mint for two provisions");
        assert_eq!(
            count("reuse"),
            1,
            "the second provision is recorded as a reuse"
        );
        assert_eq!(count("evict"), 0, "no sibling eviction");
        assert_eq!(count("grace"), 0, "nothing graced");
        let reuse = mine.iter().find(|v| v["event"] == "reuse").unwrap();
        assert_eq!(reuse["key_prefix"], rotation_key_prefix(&n1));
        assert_eq!(reuse["file_rewritten"], false);
        assert!(
            reuse.get("terminal_id").is_some_and(|t| t.is_null()),
            "an in-cwd binding is terminal-less by construction; the line must say so as null: {reuse}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// The reuse is strict: the on-disk nonce must be LIVE, on the bound port,
    /// and registered. Each arm below is a case where handing the new session
    /// the on-disk key would be wrong, and each must MINT exactly as before F4.
    /// The last arm additionally pins that a mint is the only recovery — an
    /// unregistered on-disk nonce is never adopted here (that would widen the
    /// accept set; adoption belongs to the boot self-heal alone).
    #[test]
    fn in_cwd_reprovision_mints_when_the_on_disk_nonce_is_not_reusable() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let new_dir = || {
            let d =
                std::env::temp_dir().join(format!("coord-mcp-f4-mint-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&d).unwrap();
            d
        };

        // (a) GRACED nonce on disk: a raw re-mint evicted+graced it, then the
        //     old key is written back (a client-side copy, a lagging editor).
        //     `proxy_nonce_is_valid` still says yes for it; reuse must not.
        let d = new_dir();
        let wd = d.to_string_lossy().to_string();
        let old = register_proxy_nonce(&wd, None);
        let _newer = register_proxy_nonce(&wd, None); // evicts + graces `old`
        assert!(proxy_nonce_is_valid(&old) && live_binding(&old).is_none());
        write_mcp_json(&wd, &coord_mcp_proxy_config_json(19876, &old));
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let minted = read_proxy_nonce(&d.join(".mcp.json")).unwrap();
        assert_ne!(
            minted, old,
            "a GRACED on-disk nonce is dying — never reused"
        );
        assert!(live_binding(&minted).is_some());
        let _ = std::fs::remove_dir_all(&d);

        // (b) port moved: the live nonce is fine but the URL is dead.
        let d = new_dir();
        let wd = d.to_string_lossy().to_string();
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let on_old_port = read_proxy_nonce(&d.join(".mcp.json")).unwrap();
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19877));
        let on_new_port = read_proxy_nonce(&d.join(".mcp.json")).unwrap();
        assert_ne!(
            on_new_port, on_old_port,
            "a moved port mints fresh (client must reconnect anyway)"
        );
        assert_eq!(read_proxy_port(&wd), Some(19877));
        let _ = std::fs::remove_dir_all(&d);

        // (c) UNREGISTERED nonce on disk (a previous runner process wrote it
        //     and nothing restored it): mint, and do NOT adopt.
        let d = new_dir();
        let wd = d.to_string_lossy().to_string();
        let stranger = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        write_mcp_json(&wd, &coord_mcp_proxy_config_json(19876, &stranger));
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let minted = read_proxy_nonce(&d.join(".mcp.json")).unwrap();
        assert_ne!(minted, stranger);
        assert!(
            !proxy_nonce_is_valid(&stranger),
            "the session path must never ADOPT an unregistered on-disk nonce — \
             that widens the accept set and is the boot self-heal's decision alone"
        );
        let _ = std::fs::remove_dir_all(&d);

        // (d) a config COPIED from another checkout: the nonce is live but
        //     bound to a different workdir. Mint for this cwd, and leave the
        //     other checkout's live nonce exactly as it was.
        let other = new_dir();
        let other_wd = other.to_string_lossy().to_string();
        provision_coord_mcp_with_jwt(&other_wd, &dev, Some(19876));
        let foreign = read_proxy_nonce(&other.join(".mcp.json")).unwrap();
        let d = new_dir();
        let wd = d.to_string_lossy().to_string();
        std::fs::copy(other.join(".mcp.json"), d.join(".mcp.json")).unwrap();
        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));
        let minted = read_proxy_nonce(&d.join(".mcp.json")).unwrap();
        assert_ne!(
            minted, foreign,
            "a nonce bound to another workdir is not this cwd's to reuse"
        );
        assert_eq!(workdir_for_nonce(&minted).as_deref(), Some(wd.as_str()));
        assert!(
            live_binding(&foreign).is_some(),
            "minting for this cwd never touches the other checkout's live nonce"
        );
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&other);
    }

    /// A reusable nonce in a file that still carries only the legacy
    /// `X-Coord-Mcp-Proxy-Key` header gets the same in-place header upgrade the
    /// boot self-heal applies (`UpgradeHeaders`): the nonce is re-emitted
    /// verbatim, nothing is minted, and the `reuse` line records the rewrite.
    #[test]
    fn in_cwd_reuse_upgrades_a_legacy_header_shape_without_rotating() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let log_dir = rotation_log_test_dir();
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-f4-legacy-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();
        let live = register_proxy_nonce(&wd, None);
        std::fs::write(
            d.join(".mcp.json"),
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:19876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{live}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(!read_static_authorization_presence(&d.join(".mcp.json")));

        provision_coord_mcp_with_jwt(&wd, &dev, Some(19876));

        let path = d.join(".mcp.json");
        assert_eq!(
            read_proxy_nonce(&path).as_deref(),
            Some(live.as_str()),
            "nonce preserved verbatim"
        );
        assert!(
            read_static_authorization_presence(&path),
            "header shape upgraded in place"
        );
        assert!(
            live_binding(&live).is_some(),
            "no rotation: the same binding is still live"
        );
        let raw = std::fs::read_to_string(log_dir.join(ROTATION_LOG_FILE)).unwrap();
        let mine: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["workdir"] == wd.as_str())
            .collect();
        assert_eq!(
            mine.iter().filter(|v| v["event"] == "mint").count(),
            1,
            "only the setup mint"
        );
        let reuse = mine
            .iter()
            .find(|v| v["event"] == "reuse")
            .expect("a reuse line");
        assert_eq!(reuse["file_rewritten"], true);
        assert_eq!(mine.iter().filter(|v| v["event"] == "evict").count(), 0);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// F4 §3: every `mint` and `evict` forensics line names the terminal of the
    /// slot it concerns, so the `(workdir, terminal_id)` grouping the eviction
    /// rule is keyed on can be read straight off the log. A terminal-less mint
    /// carries an explicit `null` (the key's real value), never an absent field.
    #[test]
    fn rotation_mint_and_evict_lines_carry_the_slot_terminal() {
        let dir = rotation_log_test_dir();
        let wd = format!("D:/rot-f4-terminal-{}", uuid::Uuid::now_v7());
        let term = format!("terminal-f4-{}", uuid::Uuid::now_v7());

        let first = register_proxy_nonce(&wd, Some(term.as_str()));
        let _second = register_proxy_nonce(&wd, Some(term.as_str())); // same slot → evicts `first`
        let _bare = register_proxy_nonce(&wd, None); // terminal-less slot → evicts nothing

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let mine: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["workdir"] == wd.as_str())
            .collect();
        let mints: Vec<&serde_json::Value> = mine.iter().filter(|v| v["event"] == "mint").collect();
        assert_eq!(mints.len(), 3);
        assert_eq!(mints[0]["terminal_id"], term.as_str());
        assert_eq!(mints[1]["terminal_id"], term.as_str());
        assert!(
            mints[2].get("terminal_id").is_some_and(|t| t.is_null()),
            "a terminal-less mint carries an explicit null: {}",
            mints[2]
        );
        let evicts: Vec<&serde_json::Value> =
            mine.iter().filter(|v| v["event"] == "evict").collect();
        assert_eq!(evicts.len(), 1, "only the same-terminal re-mint evicts");
        assert_eq!(evicts[0]["key_prefix"], rotation_key_prefix(&first));
        assert_eq!(
            evicts[0]["terminal_id"],
            term.as_str(),
            "the evict line names the superseded slot's terminal"
        );
    }

    /// Phase 3b — persist→restore round-trip: a minted nonce is mirrored to the
    /// store, and `restore_proxy_nonces_from_store` re-loads it into a fresh
    /// in-memory map so it still validates after a (simulated) restart.
    #[test]
    fn persisted_nonce_survives_restore_round_trip() {
        // Emits a `restore` forensics line, which is unfilterable by workdir —
        // serialize against the test that reads those lines back.
        let _serial = restore_forensics_lock();
        // Inject a temp-dir store directly — NO process-global env mutation, so
        // this test cannot pollute sibling tests that read the default store.
        let (store_dir, store) = temp_store("nonce");

        // Mint a nonce in the live map, then mirror the snapshot to the INJECTED
        // store (the `register_proxy_nonce` body, split across its seams).
        let workdir = store_dir.join("session-wd").to_string_lossy().to_string();
        let (nonce, snapshot) = mint_and_register_nonce(
            &workdir,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            None,
        );
        persist_proxy_nonces_with_store(&store, &snapshot);
        assert!(proxy_nonce_is_valid(&nonce));

        // It is actually on disk (independent of the in-memory map).
        let persisted = store.load_coord_mcp_nonces();
        assert_eq!(
            persisted.get(&nonce).map(|b| b.workdir.as_str()),
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
            None,
        );
        persist_proxy_nonces_with_store(&store, &snapshot);

        // Also mint a DEVICE nonce and persist.
        let dev_wd = store_dir.join("dev-wd").to_string_lossy().to_string();
        let (dev_nonce, snapshot) = mint_and_register_nonce(
            &dev_wd,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            None,
        );
        persist_proxy_nonces_with_store(&store, &snapshot);

        let persisted = store.load_coord_mcp_nonces();
        assert!(
            !persisted.contains_key(&agent_nonce),
            "an agent nonce must NEVER be persisted to the encrypted store"
        );
        assert_eq!(
            persisted.get(&dev_nonce).map(|b| b.workdir.as_str()),
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

    /// Phase 4 of plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning
    /// — the restart slot-collapse, closed.
    ///
    /// THE gate test: it goes through the REAL producer
    /// (`device_nonce_snapshot`) → persist → restore round trip rather than a
    /// hand-built store, because the failure mode this phase is most exposed to
    /// is shipping INERT — widening only the store and the restore leg while
    /// the producer keeps dropping `terminal_id` on the floor would leave every
    /// restore landing `None`, the slot-collapse untouched, and a hand-built
    /// store test passing anyway.
    ///
    /// Two bindings, one workdir, two terminals. Before the fix both restored
    /// as `(workdir, None)` — the SAME eviction slot — so one persistent mint
    /// killed both. That is the 33-deep cascade in five seconds measured
    /// against `D:\qontinui-root` on 2026-08-19.
    #[test]
    fn restored_nonces_keep_their_terminal_so_a_remint_evicts_only_one_slot() {
        let _serial = restore_forensics_lock();
        let (store_dir, store) = temp_store("terminal-slot");

        // One shared workdir, two distinct terminals — the ordinary state for
        // two panes opened in the same repo root.
        let wd = store_dir.join("shared-wd").to_string_lossy().to_string();
        let term_a = format!("term-a-{}", uuid::Uuid::now_v7());
        let term_b = format!("term-b-{}", uuid::Uuid::now_v7());
        let (a, _) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            Some(&term_a),
        );
        let (b, snapshot) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            Some(&term_b),
        );
        // THROUGH THE REAL PRODUCER — `persist_proxy_nonces_with_store` calls
        // `device_nonce_snapshot`, so an inert phase fails right here.
        persist_proxy_nonces_with_store(&store, &snapshot);

        // The terminal actually reached disk. (Assert before the restore, so a
        // producer that dropped it cannot be masked by a live in-memory hit.)
        let persisted = store.load_coord_mcp_nonces();
        assert_eq!(
            persisted.get(&a).and_then(|x| x.terminal_id.as_deref()),
            Some(term_a.as_str()),
            "the PRODUCER must write the terminal id — widening only the store \
             and the restore leg ships this phase inert"
        );
        assert_eq!(
            persisted.get(&b).and_then(|x| x.terminal_id.as_deref()),
            Some(term_b.as_str())
        );

        // Simulate the restart: both nonces leave the live map, then the boot
        // restore merges them back.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&a);
            map.remove(&b);
        }
        restore_proxy_nonces_from(&store);
        assert_eq!(
            terminal_id_for_nonce(&a).as_deref(),
            Some(term_a.as_str()),
            "a restored binding must carry the terminal it was minted for"
        );
        assert_eq!(terminal_id_for_nonce(&b).as_deref(), Some(term_b.as_str()));

        // Arm 1 — THE MEASURED CASCADE. A TERMINAL-LESS persistent mint into
        // the same workdir (the in-cwd `.mcp.json` writer and the boot
        // self-heal both mint with `None`) must now evict NEITHER restored
        // binding. Pre-Phase-4 every restored binding carried `terminal_id:
        // None`, so `None == None` matched all of them and one mint took the
        // lot — 33 of them in five seconds against `D:\qontinui-root` on
        // 2026-08-19.
        let (terminalless, _) =
            mint_and_register_nonce(&wd, ProxyPrincipal::Device, NonceLifetime::Persistent, None);
        assert!(
            live_binding(&a).is_some() && live_binding(&b).is_some(),
            "a terminal-less re-mint must not collapse the restored per-terminal \
             slots — this assertion IS the 33-deep eviction cascade"
        );

        // Arm 2 — a same-terminal re-provision still evicts its own
        // predecessor, and ONLY that one. The narrowing must not degrade into
        // "nothing is ever evicted".
        let (fresh_a, _) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            Some(&term_a),
        );
        assert!(
            live_binding(&a).is_none(),
            "terminal A's own predecessor is still evicted by its re-provision"
        );
        assert!(
            live_binding(&b).is_some(),
            "terminal B's restored nonce must SURVIVE A's re-mint"
        );
        assert!(
            live_binding(&terminalless).is_some(),
            "and so must the terminal-less binding — a different slot again"
        );

        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&a);
            map.remove(&b);
            map.remove(&fresh_a);
            map.remove(&terminalless);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 4 — a PRE-widening store (bare workdir strings) still restores,
    /// with `terminal_id: None` exactly as it behaved before. **No `.enc`
    /// migration is required**, and the legacy arm is what guarantees it: a
    /// deserialization regression here would drop every persisted device nonce
    /// on the next boot, i.e. reproduce the incident this plan closes.
    #[test]
    fn legacy_bare_string_nonce_store_restores_without_migration() {
        let _serial = restore_forensics_lock();
        let (store_dir, store) = temp_store("legacy-nonce");

        let wd = store_dir.join("legacy-wd").to_string_lossy().to_string();
        let nonce = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        // The legacy on-disk shape is written by `secure_storage`'s own
        // round-trip test (it owns `StoredTokens`); here we pin the CONSUMER
        // half — a store holding a terminal-less binding restores as one.
        store
            .store_coord_mcp_nonces(&HashMap::from([(
                nonce.clone(),
                stored_binding(&wd, None, None),
            )]))
            .expect("write the legacy-equivalent store");

        restore_proxy_nonces_from(&store);
        assert!(
            proxy_nonce_is_valid(&nonce),
            "a terminal-less persisted nonce must still restore"
        );
        assert_eq!(
            workdir_for_nonce(&nonce).as_deref(),
            Some(wd.as_str()),
            "and keep its workdir"
        );
        assert_eq!(
            terminal_id_for_nonce(&nonce),
            None,
            "a legacy entry claims no terminal — faking one would name a PTY \
             that never existed"
        );

        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&nonce);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 4 — the OQ3 filter survives the widening: no agent binding reaches
    /// the snapshot, whatever the value shape. Asserted against
    /// `device_nonce_snapshot` DIRECTLY (the producer), not merely against what
    /// lands on disk, so a future persist path that bypasses the store cannot
    /// quietly leak one.
    #[test]
    fn device_nonce_snapshot_drops_agent_and_ephemeral_nonces() {
        let wd = format!("D:/snapshot-filter-wt-{}", uuid::Uuid::now_v7());
        let term = format!("term-{}", uuid::Uuid::now_v7());
        let (device, _) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Device,
            NonceLifetime::Persistent,
            Some(&term),
        );
        let (agent, _) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Agent {
                agent_id: uuid::Uuid::new_v4(),
            },
            NonceLifetime::Persistent,
            None,
        );
        let (ephemeral, map) = mint_and_register_nonce(
            &wd,
            ProxyPrincipal::Device,
            NonceLifetime::ephemeral(),
            None,
        );

        let snapshot = device_nonce_snapshot(&map);
        assert_eq!(
            snapshot.get(&device).and_then(|b| b.terminal_id.as_deref()),
            Some(term.as_str()),
            "a persistent device binding is persisted WITH its terminal"
        );
        assert!(
            !snapshot.contains_key(&agent),
            "OQ3: an agent nonce must never reach the persisted shape"
        );
        assert!(
            !snapshot.contains_key(&ephemeral),
            "plan 2026-07-17 §1/E: an ephemeral nonce must never reach the \
             persisted shape — the store has no expiry column, so it would \
             restore UNBOUNDED"
        );

        {
            let mut m = proxy_nonces().lock().unwrap();
            m.remove(&device);
            m.remove(&agent);
            m.remove(&ephemeral);
        }
    }

    /// The BOUND on the persisted set (plan 2026-08-20 review finding 2).
    ///
    /// Phase 4 carried `terminal_id` into the store, which removed the only
    /// thing reaping it: nothing else evicts a persistent device binding
    /// (`revoke_proxy_nonce` has no production caller,
    /// `evict_proxy_nonces_for_workdir` fires only on relay-chat close, terminal
    /// close revokes nothing), so every terminal ever spawned added ONE
    /// permanent entry, restored at every boot, with no expiry and no cap.
    ///
    /// Asserted against `device_nonce_snapshot` directly — it is the single
    /// producer of the stored shape, so a bound anywhere else could be routed
    /// around. Built as a plain map rather than by minting: `mint_and_register_nonce`
    /// returns a clone of the WHOLE process-global map, which under the parallel
    /// harness carries peer nonces and would make the count nondeterministic.
    #[test]
    fn device_nonce_snapshot_is_bounded_and_drops_the_oldest_first() {
        let base = std::time::SystemTime::UNIX_EPOCH;
        let mut map: HashMap<String, NonceBinding> = HashMap::new();
        let over = MAX_PERSISTED_DEVICE_NONCES + 25;
        // `i` doubles as the age rank: index 0 is the OLDEST.
        let nonce_at = |i: usize| format!("bound-{i:06}");
        for i in 0..over {
            map.insert(
                nonce_at(i),
                NonceBinding {
                    workdir: format!("D:/bounded-wt/{i}"),
                    principal: ProxyPrincipal::Device,
                    lifetime: NonceLifetime::Persistent,
                    session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                    terminal_id: Some(format!("term-{i}")),
                    minted_at: base + std::time::Duration::from_secs(i as u64),
                },
            );
        }

        let snapshot = device_nonce_snapshot(&map);
        assert_eq!(
            snapshot.len(),
            MAX_PERSISTED_DEVICE_NONCES,
            "the persisted set must be capped"
        );
        // The 25 OLDEST are the ones dropped; every newer one survives.
        for i in 0..25 {
            assert!(
                !snapshot.contains_key(&nonce_at(i)),
                "binding {i} is among the oldest and must not be persisted"
            );
        }
        for i in 25..over {
            assert!(
                snapshot.contains_key(&nonce_at(i)),
                "binding {i} is newer than the cut and must be persisted"
            );
        }
        // Deterministic: `enqueue_nonce_persist` skips a write when the snapshot
        // equals the last one written, so a nondeterministic cut would re-encrypt
        // and rewrite the whole store on every single mint.
        assert_eq!(
            snapshot,
            device_nonce_snapshot(&map),
            "the cut must be deterministic for the same input map"
        );
        // The live map is untouched — capping decides what survives the NEXT
        // restart, never what validates right now.
        assert_eq!(map.len(), over, "the live registry is not evicted");

        // At or below the cap nothing is dropped at all.
        let mut small = map;
        while small.len() > MAX_PERSISTED_DEVICE_NONCES {
            let k = small.keys().next().unwrap().clone();
            small.remove(&k);
        }
        assert_eq!(device_nonce_snapshot(&small).len(), small.len());
    }

    /// The persisted AGE, end to end — the fix for the defect the bound
    /// introduced.
    ///
    /// With no age in the store, `restore_proxy_nonces_from` stamped every
    /// restored binding with `SystemTime::now()`, so they all TIED. The instant
    /// `restored + minted_this_process` exceeded the cap, every eviction
    /// candidate came from that tied pool and the "oldest-first" cut fell
    /// entirely to the nonce-string tiebreak — a uniformly random pick over hex
    /// strings, across a pool mixing long-dead terminals with sessions alive
    /// right now that survived the restart. The cap could drop a live session's
    /// credential to keep a year-old dead terminal's, which is exactly the
    /// orphaning it exists to prevent, and it engaged precisely when the cap
    /// did.
    ///
    /// Driven through the REAL producer (`device_nonce_snapshot`) and a REAL
    /// encrypted store, for the same anti-inert reason as the terminal-id gate
    /// test: widening only the struct and the restore leg would leave the age
    /// never WRITTEN, every restore back on `now()`, and a hand-built store
    /// test passing anyway.
    #[test]
    fn restored_bindings_carry_their_persisted_age_not_the_restore_instant() {
        let _serial = restore_forensics_lock();
        let (store_dir, store) = temp_store("nonce-age");

        // Two bindings with distinct, deliberately ANCIENT ages — far enough
        // back that a `now()` fallback is unmistakable.
        let old_secs = 1_600_000_000u64; // 2020-09-13
        let newer_secs = 1_700_000_000u64; // 2023-11-14
        let wd = store_dir.join("aged-wd").to_string_lossy().to_string();
        let n_old = format!("age-old-{}", uuid::Uuid::now_v7());
        let n_new = format!("age-new-{}", uuid::Uuid::now_v7());
        let n_legacy = format!("age-legacy-{}", uuid::Uuid::now_v7());
        let mut map: HashMap<String, NonceBinding> = HashMap::new();
        for (nonce, secs) in [(&n_old, old_secs), (&n_new, newer_secs)] {
            map.insert(
                nonce.clone(),
                NonceBinding {
                    workdir: wd.clone(),
                    principal: ProxyPrincipal::Device,
                    lifetime: NonceLifetime::Persistent,
                    session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                    terminal_id: Some(format!("term-{secs}")),
                    minted_at: minted_at_from_unix(Some(secs)),
                },
            );
        }
        // THROUGH THE REAL PRODUCER — an inert widening fails on the next two
        // assertions, before any restore can mask it.
        persist_proxy_nonces_with_store(&store, &map);

        let mut persisted = store.load_coord_mcp_nonces();
        assert_eq!(
            persisted.get(&n_old).and_then(|b| b.minted_at_unix),
            Some(old_secs),
            "the PRODUCER must write the mint time — without it the cap's \
             oldest-first cut is a coin flip after every restart"
        );
        assert_eq!(
            persisted.get(&n_new).and_then(|b| b.minted_at_unix),
            Some(newer_secs)
        );

        // Add a LEGACY entry: a store value written before the field existed.
        // No migration — it simply carries no age.
        persisted.insert(n_legacy.clone(), stored_binding(&wd, None, None));
        store.store_coord_mcp_nonces(&persisted).unwrap();
        assert_eq!(
            store.load_coord_mcp_nonces().len(),
            3,
            "a mixed dated/undated store still deserializes in full — the \
             widening is migration-free"
        );

        // The restart: the live map has none of them, then the boot restore
        // merges all three back.
        restore_proxy_nonces_from(&store);

        assert_eq!(
            live_binding(&n_old).map(|b| b.minted_at),
            Some(minted_at_from_unix(Some(old_secs))),
            "a restored binding must carry its TRUE mint time, not the restore \
             instant"
        );
        assert_eq!(
            live_binding(&n_new).map(|b| b.minted_at),
            Some(minted_at_from_unix(Some(newer_secs)))
        );
        assert_eq!(
            live_binding(&n_legacy).map(|b| b.minted_at),
            Some(std::time::SystemTime::UNIX_EPOCH),
            "an entry with no persisted age restores at UNIX_EPOCH — the honest \
             'unrecoverable, but certainly older than anything dated'. A `now()` \
             fallback would sort decade-old cruft as the NEWEST thing in the map"
        );

        // And the ages the restore recovered are what the next snapshot writes
        // back: unknown stays unknown (0), dated stays dated.
        let live_subset: HashMap<String, NonceBinding> = [&n_old, &n_new, &n_legacy]
            .into_iter()
            .filter_map(|n| live_binding(n).map(|b| (n.clone(), b)))
            .collect();
        let re_snapshot = device_nonce_snapshot(&live_subset);
        assert_eq!(
            re_snapshot.get(&n_old).and_then(|b| b.minted_at_unix),
            Some(old_secs),
            "a restored age must survive the NEXT persist unchanged"
        );
        assert_eq!(
            re_snapshot.get(&n_legacy).and_then(|b| b.minted_at_unix),
            Some(0),
            "an unknown age re-persists as the 0 sentinel — it must never be \
             laundered into 'minted just now' by a rewrite"
        );

        {
            let mut m = proxy_nonces().lock().unwrap();
            m.remove(&n_old);
            m.remove(&n_new);
            m.remove(&n_legacy);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// The cut's ordering rule for the two age classes, at the cap.
    ///
    /// A pre-widening store restores every entry with NO age. Those are the
    /// oldest things on the box by construction (they were written by a binary
    /// that predates the field), so they must be cut BEFORE anything carrying a
    /// real timestamp — never after. Asserted at the cap boundary, since that
    /// is the only place the ordering has any effect.
    ///
    /// Built as a plain map rather than by minting, for the same reason as the
    /// bound test: `mint_and_register_nonce` returns the whole process-global
    /// map, which the parallel harness makes nondeterministic.
    #[test]
    fn unknown_age_bindings_are_cut_before_dated_ones() {
        let dated = 40usize;
        let undated = MAX_PERSISTED_DEVICE_NONCES - dated + 14; // 14 over the cap
        let mut map: HashMap<String, NonceBinding> = HashMap::new();
        let binding = |i: usize, minted_at: std::time::SystemTime| NonceBinding {
            workdir: format!("D:/age-class/{i}"),
            principal: ProxyPrincipal::Device,
            lifetime: NonceLifetime::Persistent,
            session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
            terminal_id: Some(format!("term-{i}")),
            minted_at,
        };
        // The undated pool: a legacy store's restore, all at the sentinel.
        let undated_nonce = |i: usize| format!("undated-{i:06}");
        for i in 0..undated {
            map.insert(undated_nonce(i), binding(i, minted_at_from_unix(None)));
        }
        // The dated pool — every one of them ANCIENT (2001), so this cannot
        // pass by accident on recency: the only thing that keeps them is being
        // dated at all.
        let dated_nonce = |i: usize| format!("dated-{i:06}");
        for i in 0..dated {
            map.insert(
                dated_nonce(i),
                binding(i, minted_at_from_unix(Some(1_000_000_000 + i as u64))),
            );
        }

        let snapshot = device_nonce_snapshot(&map);
        assert_eq!(snapshot.len(), MAX_PERSISTED_DEVICE_NONCES);
        for i in 0..dated {
            assert!(
                snapshot.contains_key(&dated_nonce(i)),
                "a DATED binding must outlive every unknown-age one — an entry \
                 with no persisted age predates the field, so it is older than \
                 anything that has one"
            );
        }
        let surviving_undated = (0..undated)
            .filter(|i| snapshot.contains_key(&undated_nonce(*i)))
            .count();
        assert_eq!(
            surviving_undated,
            MAX_PERSISTED_DEVICE_NONCES - dated,
            "the whole overflow must come out of the unknown-age pool"
        );
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
            None,
        );
        assert!(
            proxy_nonce_is_valid(&nonce),
            "minting must register the nonce in-memory regardless of persistence"
        );
    }

    /// A persistence-DISABLED `restore_proxy_nonces_from_store` returns 0 on
    /// EVERY call and does not burn the run-once restore guard.
    ///
    /// Both properties were broken when the guard moved ahead of the disabled
    /// check: the second disabled call returned the live map size (which under
    /// the parallel harness is whatever peers happen to have minted),
    /// contradicting this function's own "0 when persistence is disabled"
    /// contract, and a process that later flipped `COORD_MCP_PERSIST_NONCES`
    /// on could never restore at all. Neither is reachable from today's single
    /// boot-task caller — which is why it is worth a test rather than a
    /// comment.
    ///
    /// Test builds default persistence OFF (see `nonce_persistence_enabled`),
    /// so this exercises the real arm with no env mutation.
    #[test]
    fn disabled_restore_returns_zero_every_time_and_keeps_the_guard_unburnt() {
        // The first call emits the aggregate `restore` line.
        let _serial = restore_forensics_lock();
        assert!(!nonce_persistence_enabled(), "test-build precondition");

        assert_eq!(
            restore_proxy_nonces_from_store(),
            NonceRestoreOutcome::default()
        );
        assert_eq!(
            restore_proxy_nonces_from_store(),
            NonceRestoreOutcome::default(),
            "a repeated disabled call must still report 0 on BOTH counts, not the \
             live map size"
        );
        assert!(
            PROXY_NONCES_RESTORED.get().is_none(),
            "a disabled call must leave the restore available — burning the \
             guard here makes a later enabled call a permanent no-op"
        );
    }

    /// Plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 3 —
    /// the restore reports what it RECOVERED, separately from the live map size.
    ///
    /// This replays the 2026-08-24 boot's shape exactly: the store held one
    /// binding and that binding was already live, so nothing was recovered. The
    /// old return value was the live map size, printed by the boot task under
    /// the word `restored` — so the line read `restored 1` for a boot whose
    /// recovery was 0. Paired with `root self-heal = AdoptNonce` that reads as a
    /// healthy boot, and it meant the "restored 0 followed by a root Rewrite"
    /// smell test could never fire, because the printed number was never the
    /// recovered count.
    #[test]
    fn restore_reports_what_it_recovered_not_the_live_map_size() {
        let _serial = restore_forensics_lock();
        let (store_dir, store) = temp_store("restore-honest");
        let wd = store_dir.join("already-live").to_string_lossy().to_string();

        // The 2026-08-24 shape: one persisted entry, already in the live map.
        let live_nonce = register_proxy_nonce(&wd, None);
        let mut persisted = HashMap::new();
        persisted.insert(
            live_nonce.clone(),
            stored_binding(&wd, None, Some(1_600_000_000)),
        );
        store.store_coord_mcp_nonces(&persisted).unwrap();

        let outcome = restore_proxy_nonces_from(&store);
        assert_eq!(
            outcome.inserted, 0,
            "the restore recovered NOTHING — the only persisted entry was \
             already live. This is the number the boot summary must print"
        );
        assert!(
            outcome.live_map_len >= 1,
            "the live map size is a different, non-zero number — which is \
             exactly why printing it as `restored` hid the incident"
        );

        // A genuinely absent entry IS counted, so the corrected number is not
        // simply pinned to zero.
        let missing = format!("restore-honest-{}", uuid::Uuid::new_v4().simple());
        persisted.insert(
            missing.clone(),
            stored_binding(&wd, None, Some(1_600_000_100)),
        );
        store.store_coord_mcp_nonces(&persisted).unwrap();
        let outcome = restore_proxy_nonces_from(&store);
        assert_eq!(
            outcome.inserted, 1,
            "exactly the one entry the live map lacked is counted as recovered"
        );

        {
            let mut m = proxy_nonces().lock().unwrap();
            m.remove(&live_nonce);
            m.remove(&missing);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 3c — the pure reconcile predicate: rewrite on a port mismatch,
    /// and (post-#1079 follow-up) upgrade the header shape in place when the
    /// port matches but the file is still legacy-only.
    #[test]
    fn reconcile_action_rewrites_only_on_port_mismatch() {
        // A port mismatch wins over every header consideration: that arm
        // rewrites through the current emitter, so it already yields the new
        // shape and must not be split by it.
        assert_eq!(
            reconcile_action(Some(9877), Some("old"), true, false, 9876, false),
            ReconcileAction::Rewrite,
            "stale port → rewrite"
        );
        assert_eq!(
            reconcile_action(Some(9877), Some("old"), true, true, 9876, false),
            ReconcileAction::Rewrite,
            "stale port → rewrite even when the header shape is already current"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some("keep"), true, true, 9876, false),
            ReconcileAction::Leave,
            "matching port + registered nonce + current header shape → leave"
        );
        assert_eq!(
            reconcile_action(None, None, false, false, 9876, false),
            ReconcileAction::Leave,
            "no readable proxy port (absent / static-bearer agent config) → leave"
        );
    }

    /// Plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 2
    /// — the arm ORDERING of the session resolver, which must mirror
    /// [`root_reconcile_action`] exactly.
    ///
    /// The load-bearing case is `AdoptNonce` beating `UpgradeHeaders`. A config
    /// left by a previous process is typically BOTH stale-credentialed and
    /// legacy-shaped, and the two repairs are not interchangeable: the upgrade
    /// rewrites the file around a nonce that still does not validate — no help
    /// to the live client, and it burns the one chance to leave the file
    /// byte-identical. Order them the other way and the measured incident (10 of
    /// 11 open workdirs holding a 401ing nonce) is repaired into a *rewritten*
    /// file that still 401s.
    ///
    /// Also pins the secondary-instance question the plan closed as OQ3: a
    /// secondary must never adopt a nonce the PRIMARY wrote, and it never can,
    /// because every adopt arm requires `port == this instance's bound port`.
    /// No separate `owns_shared_root_state` check is needed or wanted — it would
    /// be a second, weaker spelling of a condition the port comparison already
    /// enforces exactly.
    #[test]
    fn reconcile_action_adopts_an_unregistered_nonce_ahead_of_the_header_upgrade() {
        assert_eq!(
            reconcile_action(Some(9876), Some("stale"), false, false, 9876, false),
            ReconcileAction::AdoptNonce,
            "unregistered nonce + LEGACY header shape → adopt, NOT upgrade: the \
             credential is what a live client is failing on, and the adopt arm \
             is the only one that leaves the file byte-identical"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some("stale"), false, true, 9876, false),
            ReconcileAction::AdoptNonce,
            "unregistered nonce + CURRENT header shape → adopt (nothing else to do)"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some("live"), true, false, 9876, false),
            ReconcileAction::UpgradeHeaders,
            "a REGISTERED nonce with a legacy shape is still an upgrade — adoption \
             must not swallow the arm it precedes"
        );
        // The port comparison outranks adoption: a config naming a port this
        // instance does not own belongs to a different registry, and adopting
        // its nonce here would register a credential into the wrong process.
        assert_eq!(
            reconcile_action(Some(9877), Some("stale"), false, false, 9876, false),
            ReconcileAction::Rewrite,
            "port mismatch beats adoption — the client's cached URL is stale too"
        );
        // Nothing to adopt: an absent or empty nonce is not a credential.
        assert_eq!(
            reconcile_action(Some(9876), None, false, false, 9876, false),
            ReconcileAction::Leave,
            "no readable nonce → nothing to adopt and nothing to preserve"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some(""), false, false, 9876, false),
            ReconcileAction::Leave,
            "an empty nonce must never be adopted as a credential"
        );
    }

    /// The session-side header upgrade — the residual plan
    /// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning` recorded
    /// against `reconcile_session_configs` ("still keys on port alone, so a
    /// live session workdir's `.mcp.json` gets no header upgrade at boot").
    ///
    /// A legacy-only config on the RIGHT port is the DCR-escalating shape: it
    /// authenticates perfectly, so every credential-shaped predicate calls it
    /// healthy, and the next client launched against it still escalates a 401
    /// into OAuth/DCR. That is why the arm keys on the header SHAPE and not on
    /// the nonce's health.
    #[test]
    fn reconcile_action_upgrades_a_legacy_only_config_on_the_bound_port() {
        assert_eq!(
            reconcile_action(Some(9876), Some("keep"), true, false, 9876, false),
            ReconcileAction::UpgradeHeaders,
            "matching port, REGISTERED nonce, legacy-only headers → upgrade in place"
        );
        // No nonce to re-emit ⇒ nothing to preserve, and rewriting the shape
        // around an empty credential would produce a config that authenticates
        // against nothing. Left alone rather than upgraded or rotated.
        assert_eq!(
            reconcile_action(Some(9876), None, false, false, 9876, false),
            ReconcileAction::Leave,
            "matching port but no readable nonce → leave (nothing to preserve)"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some(""), false, false, 9876, false),
            ReconcileAction::Leave,
            "an empty nonce is not a credential to preserve"
        );
    }

    /// Phase 3c — end-to-end reconcile over a workdir set: a stale-port proxy
    /// config is rewritten to the bound port; a legacy-only config on the right
    /// port whose nonce is REGISTERED is upgraded in place with its nonce
    /// preserved; a config whose nonce is UNREGISTERED is adopted without any
    /// file write (plan `2026-08-25-…` Phase 2); an already-current config and
    /// an agent (static-bearer) config are left untouched.
    #[test]
    fn reconcile_session_configs_rewrites_stale_leaves_agent() {
        let _env_lock = env_lock();
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        // Nonce strings are per-run unique: the live registry is process-global
        // and shared with every parallel test, so literals would let a peer's
        // eviction decide this test's outcome.
        let run = uuid::Uuid::now_v7().simple().to_string();
        let n_keep = format!("keep-{run}");
        let n_keepauth = format!("keepauth-{run}");
        let n_adopt = format!("adopt-{run}");
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

        // Legacy-only proxy config on the RIGHT port → must be UPGRADED in
        // place: same port, same nonce, headers gain `Authorization`. Before
        // the #1079 follow-up this classified `Leave` and stayed
        // DCR-escalating.
        let legacy = base.join("legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join(".mcp.json"),
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{n_keep}"}}}}}}}}"#
            ),
        )
        .unwrap();

        // Already-current proxy config on :9876 → must be left BYTE-IDENTICAL.
        // The upgrade arm must be a one-shot repair, not a rewrite on every
        // boot: a file rewritten each time would churn the encrypted store and
        // the rotation log for no change at all.
        let ok = base.join("ok");
        std::fs::create_dir_all(&ok).unwrap();
        let ok_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n_keepauth}","X-Coord-Mcp-Proxy-Key":"{n_keepauth}"}}}}}}}}"#
        );
        std::fs::write(ok.join(".mcp.json"), &ok_cfg).unwrap();

        // Current-shape proxy config on :9876 whose nonce was written by a
        // PREVIOUS process and is NOT in the live registry → the Phase 2 adopt
        // arm: re-register the exact string, write nothing. Before the arm
        // existed this classified `Leave` and the client that cached the nonce
        // 401ed forever.
        let adopt = base.join("adopt");
        std::fs::create_dir_all(&adopt).unwrap();
        let adopt_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n_adopt}","X-Coord-Mcp-Proxy-Key":"{n_adopt}"}}}}}}}}"#
        );
        std::fs::write(adopt.join(".mcp.json"), &adopt_cfg).unwrap();
        assert!(
            !proxy_nonce_is_valid(&n_adopt),
            "precondition: the adopt subject's nonce is not registered"
        );

        // The `legacy` and `ok` subjects exercise the arms that assume a
        // HEALTHY credential, so register their nonces first — otherwise the
        // adopt arm (which now precedes both) claims them and neither arm is
        // exercised at all.
        adopt_on_disk_nonce(
            &legacy.to_string_lossy(),
            &n_keep,
            false,
            std::time::SystemTime::UNIX_EPOCH,
        );
        adopt_on_disk_nonce(
            &ok.to_string_lossy(),
            &n_keepauth,
            false,
            std::time::SystemTime::UNIX_EPOCH,
        );

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
            legacy.to_string_lossy().to_string(),
            ok.to_string_lossy().to_string(),
            adopt.to_string_lossy().to_string(),
            agent.to_string_lossy().to_string(),
        ];
        let counts = reconcile_session_configs(workdirs, 9876);
        assert_eq!(
            counts,
            SessionReconcileCounts {
                rewritten: 1,
                upgraded: 1,
                adopted: 1,
                // The `agent` subject here is the STATIC-BEARER shape: no proxy
                // URL, so the resolver leaves it on port grounds and the marker
                // guard never fires. A refusal is counted only when the marker
                // is what turned a real repair away.
                refused_agent_marked: 0,
            },
            "exactly the stale-port config rotates, exactly the legacy-only one \
             is upgraded in place, and exactly the unregistered-nonce one is adopted"
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

        // The legacy-only config: port AND nonce unchanged, headers upgraded.
        // Preserving the nonce is what makes this safe — a live MCP client
        // cached it at launch and never re-reads the file, so rewriting the
        // bytes around an unchanged credential cannot strand it.
        assert_eq!(read_proxy_port(&legacy.to_string_lossy()), Some(9876));
        let up: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(legacy.join(".mcp.json")).unwrap())
                .unwrap();
        let up_headers = &up["mcpServers"]["coord-mcp"]["headers"];
        assert_eq!(
            up_headers[PROXY_AUTHORIZATION_HEADER_JSON],
            serde_json::Value::from(format!("{PROXY_BEARER_PREFIX}{n_keep}")),
            "the upgrade must add the static Authorization key carrying the SAME nonce"
        );
        assert_eq!(
            up_headers[COORD_MCP_PROXY_KEY_HEADER_JSON],
            serde_json::Value::from(n_keep.clone()),
            "and must not rotate the nonce, nor drop the legacy header the \
             recovery doors still read by name"
        );

        // The adopted config: file BYTE-IDENTICAL, nonce now VALIDATING. Both
        // halves matter — a repair that rewrote the file would strand the very
        // client the adoption exists to rescue, and one that left the nonce
        // unregistered would be a no-op with a counter attached.
        assert_eq!(
            std::fs::read_to_string(adopt.join(".mcp.json")).unwrap(),
            adopt_cfg,
            "adoption must leave the file byte-identical — a live MCP client \
             cached this nonce at connect and never re-reads the file"
        );
        assert!(
            proxy_nonce_is_valid(&n_adopt),
            "the adopted nonce must now validate against the live registry"
        );

        // Already-current config untouched BYTE-FOR-BYTE; agent config
        // preserved verbatim.
        assert_eq!(
            std::fs::read_to_string(ok.join(".mcp.json")).unwrap(),
            ok_cfg,
            "a config already carrying the non-escalating shape must not be \
             rewritten on every boot"
        );
        assert_eq!(
            std::fs::read_to_string(agent.join(".mcp.json")).unwrap(),
            agent_cfg,
            "an agent static-bearer config must never be clobbered by the reconcile"
        );

        // Drop this test's bindings out of the process-global registry so they
        // do not accumulate into a sibling test's snapshot.
        {
            let mut m = proxy_nonces().lock().unwrap();
            for n in [&n_keep, &n_keepauth, &n_adopt] {
                m.remove(n);
            }
            m.remove(new_nonce);
        }

        let _ = std::fs::remove_dir_all(&base);
        match prev_root {
            Some(p) => std::env::set_var("QONTINUI_ROOT", p),
            None => std::env::remove_var("QONTINUI_ROOT"),
        }
    }

    /// **Security regression pin.** An AGENT-class `.mcp.json` sitting on the
    /// BOUND port with an unregistered nonce matches the adopt predicate
    /// exactly — it is the shape the arm was built for — and adopting it would
    /// re-register an agent-scoped credential as Device/Persistent, after which
    /// [`proxy_principal_for_nonce`] answers `Device` and the proxy injects the
    /// live DEVICE JWT for it. That is a scope elevation, and before the
    /// principal-class marker it was unavoidable: the agent and device documents
    /// were byte-identical and a lifecycle record carries no principal-class
    /// field to disambiguate them.
    ///
    /// Asserted three ways so no single layer can regress silently — the
    /// EMITTER stamps the marker, the pure RESOLVER refuses on it, and the
    /// end-to-end reconcile leaves the nonce unregistered.
    #[test]
    fn an_agent_marked_config_is_never_adopted() {
        let tmp =
            std::env::temp_dir().join(format!("coord-mcp-agentmark-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&tmp).unwrap();
        let wd = tmp.to_string_lossy().to_string();
        let agent_id = uuid::Uuid::new_v4();

        // (1) The EMITTER self-identifies. Written through the real writer, so
        // this cannot pass against a hand-rolled document the production path
        // never produces.
        write_coord_mcp_agent_proxy_config(&wd, 9876, agent_id);
        let path = tmp.join(".mcp.json");
        let nonce = read_proxy_nonce(&path).expect("the agent proxy shape carries a nonce");
        assert!(
            read_agent_principal_marker(&path),
            "write_coord_mcp_agent_proxy_config must stamp the principal-class marker"
        );
        assert_eq!(
            proxy_principal_for_nonce(&nonce),
            Some(ProxyPrincipal::Agent { agent_id }),
            "precondition: the freshly written nonce is AGENT-scoped"
        );
        // The marker is inert to the readers the reconcile keys on.
        assert_eq!(read_proxy_port(&wd), Some(9876));
        assert!(read_static_authorization_presence(&path));

        // (2) The pure RESOLVER refuses. Same inputs that yield AdoptNonce for a
        // device config on the bound port; only the marker differs.
        assert_eq!(
            reconcile_action(Some(9876), Some("agent-nonce"), false, true, 9876, false),
            ReconcileAction::AdoptNonce,
            "control: without the marker these exact inputs ARE the adopt case"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some("agent-nonce"), false, true, 9876, true),
            ReconcileAction::Leave,
            "an agent-marked config on the bound port with an unregistered nonce must \
             be LEFT, never adopted — adoption would re-register an agent-scoped \
             credential as Device/Persistent"
        );
        // ... and it is refused in every direction, not only adoption: a rewrite
        // would hand the agent's own client a DEVICE credential instead.
        assert_eq!(
            reconcile_action(Some(9999), Some("agent-nonce"), false, true, 9876, true),
            ReconcileAction::Leave,
            "a marked config on a STALE port is left too — a rewrite emits the device shape"
        );
        assert_eq!(
            reconcile_action(Some(9876), Some("agent-nonce"), true, false, 9876, true),
            ReconcileAction::Leave,
            "a marked config is not header-upgraded either — the upgrade re-emits \
             through the DEVICE producer, which would strip the marker itself"
        );
        assert_eq!(
            root_reconcile_action(Some(9876), Some("agent-nonce"), false, true, 9876, true),
            RootReconcileAction::Leave,
            "the root resolver refuses on the same grounds — the agent writer takes \
             whatever workdir it is given, including the umbrella root"
        );

        // (3) END-TO-END over the real file. The nonce is evicted from the live
        // registry first, which is exactly what a restart leaves behind: an
        // agent nonce is never persisted, so it is NEVER registered in the next
        // process — the precondition the adopt arm keys on.
        {
            let mut m = proxy_nonces().lock().unwrap();
            m.remove(&nonce);
        }
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "precondition: a restart leaves an agent nonce unregistered"
        );
        let before = std::fs::read_to_string(&path).unwrap();

        let counts = reconcile_session_configs(vec![wd.clone()], 9876);

        assert_eq!(
            counts,
            SessionReconcileCounts {
                rewritten: 0,
                upgraded: 0,
                adopted: 0,
                // The refusal is COUNTED, not merely warned. The three effect
                // counters stay zero — that is the property this test is about
                // and it is unchanged — while this fourth field records that a
                // real repair (the adopt this test's precondition sets up) was
                // turned away by the marker. Zero here would mean the guard
                // never fired, which for THIS fixture would be the bug.
                refused_agent_marked: 1,
            },
            "an agent-marked config must produce no reconcile EFFECT, and must be              counted as a refusal so the boot line can report it"
        );
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "THE regression: the agent nonce must NOT have been re-registered. If it \
             validates here, `proxy_principal_for_nonce` answers Device for a credential \
             scoped to one agent, and the proxy injects the device JWT for it"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "and the file is left untouched"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Plan `2026-08-25-boot-adopt-session-nonces-across-all-workdirs` Phase 5 —
    /// the fix pinned at FLEET SCALE, over more session configs than the
    /// persisted set can hold.
    ///
    /// Stands up `N > MAX_PERSISTED_DEVICE_NONCES` session `.mcp.json` files as
    /// a "previous process" would have left them — right port, current header
    /// shape, a nonce this process never registered — and asserts three things
    /// that only all hold after Phases 2 AND 4:
    ///
    /// 1. **every one is adopted** (against the pre-Phase-2 `Leave`, which
    ///    adopted none and is what left 10 of 11 open workdirs 401ing on the
    ///    incident box);
    /// 2. **not one file is rewritten** — asserted on the exact bytes, because
    ///    the whole value of the arm is that a live MCP client's CACHED nonce
    ///    keeps validating, and any rewrite (even a byte-different one on the
    ///    same port) strands that client;
    /// 3. **the persisted cut keeps the newest by `.mcp.json` mtime.** This is
    ///    the assertion the vet note demanded be on AGE rather than on survival
    ///    counts: `adopt_on_disk_nonce` used to stamp `SystemTime::now()`, so an
    ///    adopted binding's age recorded the ADOPTION INSTANT and outranked
    ///    restored bindings carrying their true persisted age in
    ///    [`device_nonce_snapshot`]'s newest-first cut. A test asserting only
    ///    "all adopted, 256 survive" passes against that `now()` stamp and
    ///    misses the inversion entirely — the count is right and the WRONG 256
    ///    survive.
    ///
    ///    Which is why the fixture assigns mtimes **anti-correlated** with
    ///    adoption order. Under the `now()` stamp `minted_at` necessarily
    ///    ascends with the loop index; if the fixture's mtimes ascended with it
    ///    too, both stamps would select the same 256 files and the identity
    ///    assertion would be satisfied by the bug. Running the mtimes backwards
    ///    forces the two orderings to disagree, so the surviving SET — not just
    ///    its size — is what distinguishes them.
    ///
    /// The re-provisioner that confounds the digest gate in production
    /// (112 mint+write pairs inside the 2026-08-24 boot window, rewriting the
    /// same files for an unrelated reason) is not running here, which is why the
    /// plan directed the byte-identity gate at this harness rather than at a
    /// live workdir.
    #[test]
    fn session_configs_are_adopted_unrewritten_and_age_ordered_at_fleet_scale() {
        // Deliberately over the cap so the cut actually engages: 256 + 44.
        const N: usize = MAX_PERSISTED_DEVICE_NONCES + 44;

        let run = uuid::Uuid::now_v7().simple().to_string();
        let base = std::env::temp_dir().join(format!("coord-mcp-fleet-{run}"));
        std::fs::create_dir_all(&base).unwrap();

        // Distinct mtimes assigned ANTI-CORRELATED with adoption order: index 0
        // is the NEWEST file and index N-1 the oldest, while
        // `reconcile_session_configs` adopts them in ascending index order. So
        // "the newest 256" is exactly indices 0..256 and the expectation is
        // computable rather than observed.
        //
        // That inversion is the whole point of this fixture, and an earlier
        // version got it wrong. With mtimes INCREASING in `i` the two orderings
        // agree: under the old `SystemTime::now()` stamp `minted_at` also
        // increases with `i`, so the surviving newest-256 set is identical
        // either way and the set-identity assertion below passes against the
        // very bug it names. Anti-correlated, the `now()` ordering keeps
        // N-256..N and the mtime ordering keeps 0..256 — the two disagree on
        // 212 of 256 entries, so the assertion can only pass on the mtime stamp.
        let base_secs: i64 = 1_600_000_000; // 2020-09-13
        let mut workdirs: Vec<String> = Vec::with_capacity(N);
        let mut nonces: Vec<String> = Vec::with_capacity(N);
        let mut bytes_before: Vec<String> = Vec::with_capacity(N);
        let mut mtime_secs: Vec<u64> = Vec::with_capacity(N);

        for i in 0..N {
            let dir = base.join(format!("wd-{i:04}"));
            std::fs::create_dir_all(&dir).unwrap();
            let nonce = format!("fleet-{run}-{i:04}");
            // The CURRENT header shape — so a `Leave`/`UpgradeHeaders` outcome
            // cannot be mistaken for the adopt this test is about.
            let cfg = format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {nonce}","X-Coord-Mcp-Proxy-Key":"{nonce}"}}}}}}}}"#
            );
            let path = dir.join(".mcp.json");
            std::fs::write(&path, &cfg).unwrap();
            let secs = base_secs + (N - 1 - i) as i64 * 60;
            filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(secs, 0)).unwrap();

            assert!(
                !proxy_nonce_is_valid(&nonce),
                "precondition: nothing in this fleet is registered yet"
            );
            workdirs.push(dir.to_string_lossy().to_string());
            nonces.push(nonce);
            bytes_before.push(cfg);
            mtime_secs.push(secs as u64);
        }

        let counts = reconcile_session_configs(workdirs.clone(), 9876);

        // (1) Every config adopted; nothing rotated, nothing upgraded.
        assert_eq!(
            counts,
            SessionReconcileCounts {
                rewritten: 0,
                upgraded: 0,
                adopted: N,
                refused_agent_marked: 0,
            },
            "every previous-process session config must be ADOPTED — this is the \
             assertion that fails against the pre-Phase-2 `Leave`-only resolver"
        );

        for i in 0..N {
            // (2) Byte-identical files.
            assert_eq!(
                std::fs::read_to_string(Path::new(&workdirs[i]).join(".mcp.json")).unwrap(),
                bytes_before[i],
                "adoption must not rewrite {} — a rewrite strands the live client \
                 whose cached nonce this arm exists to rescue",
                workdirs[i]
            );
            // ... and a nonce that now validates.
            assert!(
                proxy_nonce_is_valid(&nonces[i]),
                "adopted nonce {} must validate against the live registry",
                nonces[i]
            );
            // ... carrying the FILE's age, not the adoption instant.
            let binding = live_binding(&nonces[i]).expect("adopted binding present");
            assert_eq!(
                minted_at_to_unix(binding.minted_at),
                mtime_secs[i],
                "the adopted binding must carry the `.mcp.json` mtime as its age \
                 — a `now()` stamp is what inverted the persisted cut"
            );
        }

        // (3) The persisted cut, over exactly this fleet's bindings. Snapshotting
        // a subset rather than the global map keeps the assertion deterministic
        // under the parallel harness — peers mint into the same registry.
        let fleet: HashMap<String, NonceBinding> = nonces
            .iter()
            .filter_map(|n| live_binding(n).map(|b| (n.clone(), b)))
            .collect();
        assert_eq!(fleet.len(), N, "precondition: all N bindings are live");
        let persisted = device_nonce_snapshot(&fleet);
        assert_eq!(
            persisted.len(),
            MAX_PERSISTED_DEVICE_NONCES,
            "the cap must engage — a fleet under it would not exercise the cut"
        );
        // The newest files are the LOW indices (the fixture runs mtimes
        // backwards), so the survivors are `0..256` — which is very nearly the
        // set the `now()` stamp would have DROPPED.
        let expected_survivors: std::collections::HashSet<&String> =
            nonces[..MAX_PERSISTED_DEVICE_NONCES].iter().collect();
        let actual_survivors: std::collections::HashSet<&String> = persisted.keys().collect();
        assert_eq!(
            actual_survivors, expected_survivors,
            "the persisted set must be the NEWEST-by-mtime {MAX_PERSISTED_DEVICE_NONCES}. \
             The fixture assigns mtimes ANTI-CORRELATED with adoption order, so under the \
             old `now()` stamp `minted_at` ascends with the loop index and the cut keeps \
             the LAST 256 adopted instead — the right COUNT over the wrong entries. That \
             is why this asserts identity rather than length, and why the \
             anti-correlation is load-bearing: with mtimes ascending in `i` the two \
             orderings agree and this assertion passes against the bug"
        );
        // And the ages went to disk as the file's, not the adoption's.
        for n in &nonces[..MAX_PERSISTED_DEVICE_NONCES] {
            let i: usize = n.rsplit('-').next().unwrap().parse().unwrap();
            assert_eq!(
                persisted.get(n).and_then(|b| b.minted_at_unix),
                Some(mtime_secs[i]),
                "a surviving binding must persist the file's age"
            );
        }

        {
            let mut m = proxy_nonces().lock().unwrap();
            for n in &nonces {
                m.remove(n);
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Phase 1 — the three-way on-disk census. The classification is what keeps
    /// the boot summary from overstating the fix's reach: on the incident box
    /// the whole-disk count was 591 and the reachable (open-record) count 11, so
    /// a line printing only the former would be off by ~54x.
    ///
    /// Also pins the path-key normalization, without which every open workdir
    /// classifies as `orphaned` on Windows (records carry `\`-separated,
    /// arbitrarily-cased strings; the walk produces `PathBuf`s) and the census
    /// becomes a confident lie in the direction that hides the problem.
    #[test]
    fn on_disk_config_census_classifies_open_dead_and_orphaned() {
        let base = std::env::temp_dir().join(format!("coord-mcp-census-{}", uuid::Uuid::now_v7()));
        let cfg = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp"}}}"#;

        // Root itself carries one, plus three subdirectories.
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".mcp.json"), cfg).unwrap();
        for name in ["open-wd", "dead-wd", "orphan-wd"] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(".mcp.json"), cfg).unwrap();
        }
        // A pruned tree: its config must NOT be counted, and it is the reason
        // the boot-path walk is affordable at all.
        let pruned = base.join("node_modules").join("pkg");
        std::fs::create_dir_all(&pruned).unwrap();
        std::fs::write(pruned.join(".mcp.json"), cfg).unwrap();
        // Below the depth bound → not counted.
        let deep = base.join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(".mcp.json"), cfg).unwrap();

        // Records spelled the way a Windows lifecycle record spells them:
        // backslashes and a different case.
        let open_wd = base
            .join("open-wd")
            .to_string_lossy()
            .replace('/', "\\")
            .to_uppercase();
        let dead_wd = base
            .join("dead-wd")
            .to_string_lossy()
            .replace('/', "\\")
            .to_uppercase();
        let root_wd = base.to_string_lossy().to_string();

        let census = census_on_disk_mcp_configs_at(
            &base,
            [open_wd.as_str(), root_wd.as_str()],
            [open_wd.as_str(), root_wd.as_str(), dead_wd.as_str()],
        );

        assert_eq!(
            census.total, 4,
            "root + three children; the pruned tree and the over-deep one are \
             excluded by construction, not by accident"
        );
        assert_eq!(census.open_backed, 2, "root and open-wd back OPEN records");
        assert_eq!(
            census.dead_backed, 1,
            "dead-wd is named by a record that is no longer open"
        );
        assert_eq!(census.orphaned, 1, "orphan-wd is named by no record at all");
        assert_eq!(
            census.open_backed + census.dead_backed + census.orphaned,
            census.total,
            "the three classes must total the file count — Phase 1's own gate"
        );

        let _ = std::fs::remove_dir_all(&base);
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
        // And no baked static TOKEN: `Authorization` is present (Phase 2 — the
        // OAuth-provider suppressor) but carries the freshly minted nonce.
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            v["mcpServers"]["coord-mcp"]["headers"]["Authorization"],
            serde_json::Value::from(format!("Bearer {new_nonce}"))
        );

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
        // keeps validating.
        //
        // The invariant is **the NONCE is preserved**, not the file's bytes —
        // that is what a live client cached, and the file is what the NEXT
        // client reads. This fixture is written in the Phase 2 shape so the
        // adopt is a pure no-file-change; the legacy-only fixture (which adopt
        // now ALSO upgrades in place, same nonce) is covered by
        // `upgrade_in_place_adds_authorization_without_rotating_the_nonce`.
        let dead = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dead).unwrap();
        let dead_body = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"Authorization":"Bearer notregistered-9c3f","X-Coord-Mcp-Proxy-Key":"notregistered-9c3f"}}}}"#;
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
        // Already the non-escalating shape, so there is nothing to upgrade and
        // the file is byte-identical — no rewrite → live client cache preserved.
        assert_eq!(
            std::fs::read_to_string(dead.join(".mcp.json")).unwrap(),
            dead_body,
            "adopt must NOT rewrite an already-current root .mcp.json"
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
        // Written in the CURRENT shape so the byte-identity assertions below
        // isolate the instance gate: a legacy-only fixture would now also be
        // header-upgraded by the primary's adopt arm, and this test is about WHO
        // may write, not about what a write does.
        let healthy = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"Authorization":"Bearer primary-owned-nonce","X-Coord-Mcp-Proxy-Key":"primary-owned-nonce"}}}}"#;
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
            root_reconcile_action(
                Some(9876),
                Some("primary-owned-nonce"),
                false,
                true,
                9877,
                false
            ),
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
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "an absent .mcp.json in a non-root workdir stays writable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Plan 2026-07-07 Change 1 — the pure `root_reconcile_action` resolver over
    /// explicit inputs, isolating the adopt-vs-rewrite-vs-leave decision from any
    /// file I/O or the process-global nonce map.
    ///
    /// The 4th argument is the header SHAPE (plan 2026-08-20): whether the file
    /// carries a static `Authorization` key at all.
    #[test]
    fn root_reconcile_action_resolves_adopt_vs_rewrite_vs_leave() {
        // Not our shape (no readable proxy port) → Leave regardless of nonce.
        assert_eq!(
            root_reconcile_action(None, Some("x"), false, true, 9876, false),
            RootReconcileAction::Leave
        );
        // Port moved → Rewrite (client's cached URL is stale too — reconnect).
        assert_eq!(
            root_reconcile_action(Some(9999), Some("x"), true, true, 9876, false),
            RootReconcileAction::Rewrite,
            "a moved port must rewrite even with a registered nonce"
        );
        // Same port, nonce readable but UNregistered → Adopt (the core fix).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), false, true, 9876, false),
            RootReconcileAction::AdoptNonce
        );
        // Same port, nonce readable AND registered AND already non-escalating
        // → Leave (healthy).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), true, true, 9876, false),
            RootReconcileAction::Leave
        );
        // Same port, NO nonce readable → Rewrite (nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), None, false, false, 9876, false),
            RootReconcileAction::Rewrite
        );
        // Same port, EMPTY nonce string → Rewrite (empty is nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), Some(""), false, false, 9876, false),
            RootReconcileAction::Rewrite
        );
    }

    /// The gap the credential-keyed resolver could not see (plan 2026-08-20
    /// review finding 3): on the very deploy that ships the Phase 2 emitter the
    /// runner rebuilds on the SAME port, so every `.mcp.json` already on disk
    /// stays legacy-only — healthy nonce, no static `Authorization`, therefore
    /// still DCR-escalating for the next client launched against it. That input
    /// used to classify as `Leave`.
    #[test]
    fn a_healthy_but_legacy_only_config_is_upgraded_not_left() {
        // The regression this pins: same port, REGISTERED nonce, legacy-only
        // header shape.
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), true, false, 9876, false),
            RootReconcileAction::UpgradeHeaders,
            "a registered nonce in a legacy-only file must be upgraded in place"
        );
        // The shape question is orthogonal to every other input: a MOVED port
        // still rewrites (fresh nonce), and an UNREGISTERED nonce is still
        // adopted — the upgrade never pre-empts either.
        assert_eq!(
            root_reconcile_action(Some(9999), Some("abc"), true, false, 9876, false),
            RootReconcileAction::Rewrite
        );
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), false, false, 9876, false),
            RootReconcileAction::AdoptNonce
        );
    }

    /// End-to-end on the FILE: a healthy legacy-only root config is rewritten in
    /// place, gains `Authorization`, and — the load-bearing half — keeps the
    /// **same nonce**. Minting here would strand every live client holding the
    /// old one, which is the failure the whole plan is about.
    #[test]
    fn upgrade_in_place_adds_authorization_without_rotating_the_nonce() {
        // Installed FIRST: file emission is off until some test asks for the
        // shared dir, so a forensics assertion at the end of a test that armed
        // it in the middle would read a file missing its own earlier lines.
        let rot_dir = rotation_log_test_dir();
        let root = std::env::temp_dir().join(format!("coord-mcp-upg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(".mcp.json");

        // A LIVE, registered nonce written in the pre-Phase-2 shape.
        let live = register_proxy_nonce(&root.to_string_lossy(), None);
        std::fs::write(
            &path,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{live}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            proxy_nonce_is_valid(&live),
            "precondition: the nonce is live"
        );
        assert!(
            !read_static_authorization_presence(&path),
            "precondition: the file is the legacy-only shape"
        );

        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::UpgradeHeaders
        );

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"]["coord-mcp"]["headers"][PROXY_AUTHORIZATION_HEADER_JSON],
            serde_json::Value::from(format!("{PROXY_BEARER_PREFIX}{live}")),
            "the upgraded file must carry the static Authorization key"
        );
        assert_eq!(
            read_proxy_nonce(&path).as_deref(),
            Some(live.as_str()),
            "the nonce must be PRESERVED — an upgrade is not a rotation"
        );
        assert!(
            proxy_nonce_is_valid(&live),
            "the live registry is untouched by a header upgrade"
        );

        // Idempotent: the upgraded file is now healthy AND non-escalating.
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::Leave,
            "an upgraded config must not be rewritten on every boot"
        );

        // And the ADOPT arm upgrades too — an adopted config was written by an
        // older runner, so it is the likeliest legacy-only shape on the box.
        let orphan = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        std::fs::write(
            &path,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{orphan}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(!proxy_nonce_is_valid(&orphan));
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::AdoptNonce
        );
        assert!(
            proxy_nonce_is_valid(&orphan),
            "the on-disk nonce is still adopted VERBATIM — the client cache contract"
        );
        assert_eq!(
            read_proxy_nonce(&path).as_deref(),
            Some(orphan.as_str()),
            "adopt + upgrade must not rotate the nonce either"
        );
        assert!(
            read_static_authorization_presence(&path),
            "the adopted config must also stop being DCR-escalating"
        );

        // The `adopt` forensics cause must say what happened to the FILE. It
        // asserted "(no file rewrite)" unconditionally, which the folded-in
        // header upgrade turned into the exact opposite of the truth — an
        // adjacent `write` line keeps the stream reconstructable, but only for
        // a reader who already distrusts the cause.
        let root_str = root.to_string_lossy().to_string();
        let log = std::fs::read_to_string(rot_dir.join(ROTATION_LOG_FILE)).unwrap();
        let adopt_causes: Vec<String> = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["workdir"] == root_str.as_str() && v["event"] == "adopt")
            .map(|v| v["cause"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            adopt_causes.len(),
            1,
            "exactly one adoption happened in this workdir"
        );
        assert!(
            adopt_causes[0].contains("REWRITTEN"),
            "the adopt cause must report the rewrite this arm performed, got {:?}",
            adopt_causes[0]
        );

        let _ = std::fs::remove_dir_all(&root);
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

        adopt_on_disk_nonce(&workdir, &nonce, false, std::time::SystemTime::UNIX_EPOCH);

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
        let a = register_proxy_nonce(&wd, None);
        assert!(proxy_nonce_is_valid(&a));
        let b = register_proxy_nonce(&wd, None);
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
    ///
    /// Extended for the TTL split (plan 2026-07-27-coord-mcp-flake-remediation,
    /// Phase 5/R3): the DEVICE arm graces for [`DEVICE_EVICTED_NONCE_GRACE_TTL`]
    /// (6h, strictly wider than the retained 90s [`AGENT_NONCE_GRACE_TTL`]
    /// bound), while an evicted AGENT nonce never even ENTERS the grace map.
    #[test]
    fn graced_nonce_expires_and_is_lazily_evicted() {
        // Arm 1 — lazy expiry (unchanged by the split).
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

        // Arm 2 — the DEVICE arm carries the WIDENED TTL.
        assert_eq!(AGENT_NONCE_GRACE_TTL, std::time::Duration::from_secs(90));
        assert_eq!(
            DEVICE_EVICTED_NONCE_GRACE_TTL,
            std::time::Duration::from_secs(6 * 60 * 60)
        );
        assert!(
            AGENT_NONCE_GRACE_TTL < DEVICE_EVICTED_NONCE_GRACE_TTL,
            "the widening applies only to the device arm"
        );
        let wd = format!("D:/grace-ttl-wt-{}", uuid::Uuid::now_v7());
        let before = std::time::Instant::now();
        let a = register_proxy_nonce(&wd, None);
        let _b = register_proxy_nonce(&wd, None); // evicts + graces `a`
        let expires_at = graced_nonces()
            .lock()
            .unwrap()
            .get(&a)
            .expect("an evicted device nonce enters the grace map")
            .expires_at;
        // Timing-sound bracket: grace is stamped at t >= `before`, so a 6h TTL
        // guarantees `expires_at >= before + 6h`; any applied TTL meaningfully
        // below 6h fails the lower bound, any above 6h fails the upper.
        assert!(
            expires_at >= before + DEVICE_EVICTED_NONCE_GRACE_TTL,
            "the device arm must apply the full widened TTL, not merely exceed 90s"
        );
        assert!(
            expires_at <= std::time::Instant::now() + DEVICE_EVICTED_NONCE_GRACE_TTL,
            "device grace stays bounded by DEVICE_EVICTED_NONCE_GRACE_TTL"
        );

        // Arm 3 — the AGENT arm: an evicted agent nonce must never even ENTER
        // the grace map (stronger than "not valid": the accept-set widening
        // structurally cannot reach the agent class).
        let awd = format!("D:/grace-ttl-agent-wt-{}", uuid::Uuid::now_v7());
        let agent_id = uuid::Uuid::new_v4();
        let a2 = register_agent_proxy_nonce(&awd, agent_id);
        let _b2 = register_agent_proxy_nonce(&awd, agent_id);
        assert!(
            !graced_nonces().lock().unwrap().contains_key(&a2),
            "an evicted AGENT nonce must never enter the grace map"
        );
        assert!(
            !proxy_nonce_is_valid(&a2),
            "an evicted AGENT nonce hard-fails closed"
        );

        // Arm 4 — the EPHEMERAL arm: a close-time eviction must never grace an
        // ephemeral device nonce (grace checks only expiry, so gracing one
        // would bypass the session-identity kill switch for the whole window).
        let ewd = format!("D:/grace-ttl-ephemeral-wt-{}", uuid::Uuid::now_v7());
        let e = register_session_proxy_nonce(&ewd);
        evict_proxy_nonces_for_workdir(&ewd);
        assert!(
            !graced_nonces().lock().unwrap().contains_key(&e),
            "an evicted EPHEMERAL device nonce must never enter the grace map"
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
        let a = register_proxy_nonce(&wd, None); // mint
        let b = register_proxy_nonce(&wd, None); // mint + evict(a) + grace(a)
        evict_proxy_nonces_for_workdir(&wd); // evict(b) + grace(b)
        let adopted = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        // adopt (nothing left to evict)
        adopt_on_disk_nonce(&wd, &adopted, false, std::time::SystemTime::UNIX_EPOCH);

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
        let live = register_proxy_nonce(&wd, None);
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
        // ...an unknown one cannot, and says so EXPLICITLY (Phase 3: the
        // pre-Phase-3 empty string was indistinguishable from "the runner did
        // not populate this field", which is why all 671 production reject
        // lines were unattributable). It is joined on `key_prefix` instead.
        assert_eq!(for_key(&stranger)["workdir"], ROTATION_UNKNOWN);

        for n in [live.as_str(), stranger.as_str()] {
            assert!(
                !raw.contains(n),
                "a reject line leaked a full nonce — prefixes only"
            );
        }
    }

    /// Phase 3 (plan 2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning):
    /// a `reject` line must attribute the dead key to a SESSION — workdir,
    /// principal class and terminal — whenever the nonce is still knowable, and
    /// say `"unknown"` in every field when it is not. Before this, every reject
    /// line in production carried `"workdir":""` and no principal or terminal at
    /// all, so the 2026-08-19 incident could not be pinned to a session.
    #[test]
    fn rotation_reject_line_carries_workdir_principal_and_terminal() {
        let dir = rotation_log_test_dir();

        // Arm 1 — a live DEVICE nonce minted for a named terminal.
        let wd = format!("D:/rot-attr-wt-{}", uuid::Uuid::now_v7());
        let term = format!("term-{}", uuid::Uuid::now_v7());
        let live = register_proxy_nonce(&wd, Some(&term));

        // Arm 2 — a live AGENT nonce (no terminal by construction).
        let awd = format!("D:/rot-attr-agent-wt-{}", uuid::Uuid::now_v7());
        let agent_nonce = register_agent_proxy_nonce(&awd, uuid::Uuid::new_v4());

        // Arm 3 — a key this runner never minted: nothing is knowable.
        let stranger = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );

        log_proxy_nonce_rejected(Some(&live), "bound but gated (401)");
        log_proxy_nonce_rejected(Some(&agent_nonce), "agent slot gone (401)");
        log_proxy_nonce_rejected(Some(&stranger), "unregistered (401)");

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let line_for = |n: &str| {
            raw.lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
                .filter(|v| v["event"] == "reject")
                .find(|v| v["key_prefix"] == rotation_key_prefix(n).as_str())
                .unwrap_or_else(|| panic!("a reject line for {}", rotation_key_prefix(n)))
        };

        let l = line_for(&live);
        assert_eq!(l["workdir"], wd.as_str(), "a known nonce names its workdir");
        assert_ne!(l["workdir"], "", "the workdir field is never left empty");
        assert_eq!(l["principal"], "device");
        assert_eq!(l["terminal_id"], term.as_str());

        let a = line_for(&agent_nonce);
        assert_eq!(a["workdir"], awd.as_str());
        assert_eq!(a["principal"], "agent");
        assert_eq!(
            a["terminal_id"], "none",
            "an agent nonce has no terminal BY CONSTRUCTION — a distinct fact \
             from 'unknown', and the line must not conflate them"
        );

        let s = line_for(&stranger);
        assert_eq!(s["workdir"], ROTATION_UNKNOWN);
        assert_eq!(s["principal"], ROTATION_UNKNOWN);
        assert_eq!(s["terminal_id"], ROTATION_UNKNOWN);

        // Every line, whatever the arm, carries the emitting process.
        for v in [&l, &a, &s] {
            assert!(v["runner_id"].is_string(), "every line names its runner");
            assert_eq!(
                v["pid"].as_u64(),
                Some(u64::from(std::process::id())),
                "every line names the emitting pid — the only field that moves \
                 across a restart of the same instance"
            );
        }

        for n in [live.as_str(), agent_nonce.as_str(), stranger.as_str()] {
            assert!(!raw.contains(n), "a reject line leaked a full nonce");
        }
    }

    /// Phase 3: the boot restore must leave a `restore` line with honest counts
    /// — on EVERY arm, the empty one included. A `restored: 0` line is the loud
    /// signal that the persisted set was dropped (the shape a store-schema
    /// deserialization regression takes); silence would read identically to a
    /// healthy boot, which is the state the 2026-08-19 log was actually in
    /// (one `adopt` line, zero restores, in 5,486 lines).
    #[test]
    fn rotation_restore_event_reports_restored_and_skipped_counts() {
        // The restore line is an AGGREGATE: it carries no single workdir and no
        // key prefix (both read `ROTATION_UNKNOWN` — a STATEMENT that there is
        // none, never `""`, which reads as "the runner failed to populate it"),
        // so it cannot be filtered to this test the way every other forensics
        // assertion here filters by its own workdir. Serialize the
        // restore-emitting tests instead and read only the window this test
        // owns — see `restore_forensics_lock`.
        let _serial = restore_forensics_lock();
        let dir = rotation_log_test_dir();
        let before = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE))
            .map(|s| s.lines().count())
            .unwrap_or(0);

        let restores_since = || -> Vec<serde_json::Value> {
            std::fs::read_to_string(dir.join(ROTATION_LOG_FILE))
                .unwrap()
                .lines()
                .skip(before)
                .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
                .filter(|v| v["event"] == "restore")
                .collect()
        };

        // Arm 1 — an EMPTY store still emits, with zeroes. This is the arm that
        // matters most: a store-schema deserialization regression drops every
        // persisted nonce, and `restored: 0` is the only way that shows up.
        let (empty_dir, empty_store) = temp_store("restore-empty");
        restore_proxy_nonces_from(&empty_store);
        let r = restores_since();
        assert_eq!(r.len(), 1, "an empty store still emits exactly one restore");
        assert_eq!(r[0]["restored"], 0);
        assert_eq!(r[0]["skipped"], 0);
        // Never `""` — the aggregate says "no single workdir/key", it does not
        // leave the field unpopulated (the 2026-08-19 reject-line ambiguity).
        assert_eq!(r[0]["workdir"], ROTATION_UNKNOWN);
        assert_eq!(r[0]["key_prefix"], ROTATION_UNKNOWN);
        assert!(
            r[0]["cause"].as_str().unwrap().contains("empty"),
            "the reason class must name the empty-store arm, got {:?}",
            r[0]["cause"]
        );

        // Arm 2 — a store written DIRECTLY with exactly two entries, one of
        // which is already live. Direct, not via `persist_proxy_nonces_with_store`:
        // a snapshot is a clone of the WHOLE live map, so under concurrent tests
        // it carries peer nonces and the counts would not be deterministic.
        let (store_dir, store) = temp_store("restore-counts");
        let mint = || {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        };
        let (a, b) = (mint(), mint());
        let wd_a = store_dir.join("wd-a").to_string_lossy().to_string();
        let wd_b = store_dir.join("wd-b").to_string_lossy().to_string();
        store
            .store_coord_mcp_nonces(&HashMap::from([
                (a.clone(), stored_binding(&wd_a, None, None)),
                (b.clone(), stored_binding(&wd_b, None, None)),
            ]))
            .expect("write the test store");
        // `b` is already live, so the restore must SKIP it and restore only `a`.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.insert(
                b.clone(),
                NonceBinding {
                    workdir: wd_b.clone(),
                    principal: ProxyPrincipal::Device,
                    lifetime: NonceLifetime::Persistent,
                    session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
                    terminal_id: None,
                    minted_at: std::time::SystemTime::now(),
                },
            );
        }
        restore_proxy_nonces_from(&store);
        let r = restores_since();
        assert_eq!(r.len(), 2, "one restore line per restore call");
        assert_eq!(r[1]["restored"], 1, "only `a` was actually re-inserted");
        assert_eq!(r[1]["skipped"], 1, "`b` was already live and was skipped");
        // Aggregate line: no key material at all — and the field says so with
        // the sentinel rather than being left empty.
        assert_eq!(r[1]["key_prefix"], ROTATION_UNKNOWN);
        assert_eq!(r[1]["workdir"], ROTATION_UNKNOWN);
        for n in [a.as_str(), b.as_str()] {
            assert!(
                !r[1].to_string().contains(n),
                "the restore line must carry no nonce"
            );
        }

        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&a);
            map.remove(&b);
        }
        let _ = std::fs::remove_dir_all(&empty_dir);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 3: an explicit revoke is the one way a key dies that leaves no
    /// other trace — no mint, no evict, no grace. It must emit its own line so
    /// a later `reject` carrying that prefix joins to something.
    #[test]
    fn rotation_revoke_line_is_emitted_for_device_and_agent() {
        let dir = rotation_log_test_dir();

        let wd = format!("D:/rot-revoke-wt-{}", uuid::Uuid::now_v7());
        let nonce = register_proxy_nonce(&wd, None);
        revoke_proxy_nonce(&nonce);

        let awd = format!("D:/rot-revoke-agent-wt-{}", uuid::Uuid::now_v7());
        let agent_id = uuid::Uuid::new_v4();
        let agent_nonce = register_agent_proxy_nonce(&awd, agent_id);
        revoke_agent_proxy_nonces(agent_id);

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let revokes: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
            .filter(|v| v["event"] == "revoke")
            .filter(|v| v["workdir"] == wd.as_str() || v["workdir"] == awd.as_str())
            .collect();
        assert_eq!(revokes.len(), 2, "one revoke line per revoked nonce");
        assert_eq!(
            revokes
                .iter()
                .find(|v| v["workdir"] == wd.as_str())
                .unwrap()["key_prefix"],
            rotation_key_prefix(&nonce).as_str()
        );
        assert_eq!(
            revokes
                .iter()
                .find(|v| v["workdir"] == awd.as_str())
                .unwrap()["key_prefix"],
            rotation_key_prefix(&agent_nonce).as_str()
        );
        for n in [nonce.as_str(), agent_nonce.as_str()] {
            assert!(!raw.contains(n), "a revoke line leaked a full nonce");
        }
    }

    /// Post-merge follow-up to plan
    /// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`, which
    /// recorded this as an explicit residual: "`release_workdir_on_session_close`
    /// also revokes nonces and still emits no `revoke` line."
    ///
    /// It matters more than the two paths Phase 3 did cover. `revoke_proxy_nonce`
    /// has NO production caller and agent nonces are never persisted, so on a
    /// real box every key that died by revocation died here — silently. A later
    /// `reject` carrying one of those prefixes joined to a `mint`/`write` pair
    /// and then to nothing, which is indistinguishable from the eviction cascade
    /// Phase 4 fixed. Telling those two apart is precisely what the 2026-08-19
    /// reconstruction needed and could not do.
    #[test]
    fn rotation_revoke_line_is_emitted_on_session_close() {
        let dir = rotation_log_test_dir();

        let wd = format!("D:/rot-close-wt-{}", uuid::Uuid::now_v7());
        // Both classes bound to one workdir: the shared `.mcp.json` credential
        // (terminal-less) and a per-PTY one. Session close drops both, so both
        // must be accounted for.
        let shared = register_proxy_nonce(&wd, None);
        let per_terminal = register_proxy_nonce(&wd, Some("terminal-rot-close"));

        release_workdir_on_session_close(&wd);

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let revokes: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON per line"))
            .filter(|v| v["event"] == "revoke" && v["workdir"] == wd.as_str())
            .collect();
        assert_eq!(
            revokes.len(),
            2,
            "one revoke line per nonce the close killed, not one per close"
        );

        let by_prefix = |n: &str| {
            revokes
                .iter()
                .find(|v| v["key_prefix"] == rotation_key_prefix(n).as_str())
                .unwrap_or_else(|| panic!("no revoke line joins to the {n} prefix"))
                .clone()
        };
        // The Phase 4 join key rides on the line, spelled the same way a
        // `reject` spells it: the string "none" for a terminal-less binding,
        // never JSON null, so the two events join on one shape.
        assert_eq!(by_prefix(&shared)["terminal_id"], "none");
        assert_eq!(
            by_prefix(&per_terminal)["terminal_id"],
            "terminal-rot-close",
            "the terminal id must be resolved BEFORE the retain drops the \
             binding — afterwards there is nothing left to resolve it from"
        );
        for v in &revokes {
            assert!(
                v["cause"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("session close"),
                "the cause must name the path, or the line cannot be told from \
                 an explicit revoke: {v}"
            );
        }
        for n in [shared.as_str(), per_terminal.as_str()] {
            assert!(!raw.contains(n), "a revoke line leaked a full nonce");
        }
    }

    /// Phase 3: the resolved rotation-log path is discoverable without guessing.
    /// The 2026-08-19 investigation searched `D:/qontinui-root` and `C:/claude`,
    /// concluded the file did not exist, and discarded 5,486 lines of the
    /// incident — it was under `%LOCALAPPDATA%` the whole time.
    #[test]
    fn rotation_log_path_is_surfaced_for_health() {
        // Under `cfg(test)` the dir override is what resolves, so installing it
        // is what makes the path non-null here — same code path as production,
        // different resolver arm.
        let dir = rotation_log_test_dir();
        let path = rotation_log_path().expect("a resolvable dir yields a path");
        assert_eq!(path, dir.join(ROTATION_LOG_FILE));

        let health = rotation_log_health_json();
        assert_eq!(health["path"], path.to_string_lossy().as_ref());
        assert!(
            health["exists"].is_boolean(),
            "`exists` is always a boolean — a non-null path with exists:false \
             means 'nothing emitted yet', NOT 'forensics are off'"
        );
        // Idempotent breadcrumb: a duplicate boot-task run must not re-log.
        log_rotation_log_path_once();
        log_rotation_log_path_once();
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

    /// Credential-hygiene Task 5 — a REVOKED nonce no longer validates, both
    /// from the live registry and from the grace registry (revocation is
    /// total; grace only survives supersession, never an explicit revoke).
    #[test]
    fn revoked_nonce_no_longer_validates_including_grace() {
        // Live revoke.
        let wd = format!("D:/revoke-wt-{}", uuid::Uuid::now_v7());
        let nonce = register_proxy_nonce(&wd, None);
        assert!(proxy_nonce_is_valid(&nonce));
        revoke_proxy_nonce(&nonce);
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "a revoked live nonce must not validate"
        );
        // Idempotent.
        revoke_proxy_nonce(&nonce);

        // Graced revoke: re-mint moves the first nonce onto grace, where it
        // still validates — an explicit revoke must kill that too.
        let wd2 = format!("D:/revoke-grace-wt-{}", uuid::Uuid::now_v7());
        let old = register_proxy_nonce(&wd2, None);
        let _new = register_proxy_nonce(&wd2, None);
        assert!(
            proxy_nonce_is_valid(&old),
            "precondition: the superseded nonce rides the grace TTL"
        );
        revoke_proxy_nonce(&old);
        assert!(
            !proxy_nonce_is_valid(&old),
            "revocation must purge the grace registry too"
        );
    }

    /// Credential-hygiene Task 5 — session-close release: revokes EVERY live
    /// nonce for the workdir and reaps each one's app-data session-restore
    /// config file. (Graced predecessors are left to expire on their own ≤90s
    /// TTL — the grace registry is keyed only by nonce.)
    ///
    /// The reap is NONCE-keyed, so the seeded fixtures must carry their nonce
    /// in band exactly as the real writer emits it — a bare `{}` placeholder
    /// carries none and is correctly left alone. Both filename classes are
    /// covered: the workdir-derived name and a per-TERMINAL one, whose name
    /// this entry point cannot recompute (it has no terminal id) and which a
    /// name-keyed reap therefore missed.
    #[test]
    fn release_workdir_on_session_close_revokes_and_reaps() {
        let cfg_body = |nonce: &str| {
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{nonce}"}}}}}}}}"#
            )
        };

        let wd = format!("D:/close-wt-{}", uuid::Uuid::now_v7());
        let current = register_proxy_nonce(&wd, None);
        let per_terminal = register_proxy_nonce(&wd, Some("terminal-close-1"));
        assert!(proxy_nonce_is_valid(&current));
        assert!(proxy_nonce_is_valid(&per_terminal));

        // Seed the app-data config files the release must reap.
        let cfg_dir = crate::session::claude_hook::session_restore_dir().join("coord-mcp");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg = cfg_dir.join(mcp_config_file_name(&wd, None));
        let cfg_terminal = cfg_dir.join(mcp_config_file_name(&wd, Some("terminal-close-1")));
        std::fs::write(&cfg, cfg_body(&current)).unwrap();
        std::fs::write(&cfg_terminal, cfg_body(&per_terminal)).unwrap();

        release_workdir_on_session_close(&wd);

        assert!(
            !proxy_nonce_is_valid(&current),
            "session close must revoke the workdir's live nonce"
        );
        assert!(
            !proxy_nonce_is_valid(&per_terminal),
            "session close must revoke the workdir's per-terminal nonce too"
        );
        assert!(
            !cfg.exists(),
            "session close must reap the workdir's session-restore config file"
        );
        assert!(
            !cfg_terminal.exists(),
            "session close must reap the PER-TERMINAL config file, whose name it \
             cannot recompute — the reap is keyed on the revoked nonce, not the name"
        );
    }

    /// Credential-hygiene Task 5 — the session-restore reaper: a config whose
    /// port is dead is reaped on start; one naming THIS runner's port is kept
    /// iff its nonce is registered; a live FOREIGN port's config is kept
    /// (another runner owns it); garbage is reaped.
    #[test]
    fn reaper_drops_dead_port_and_unregistered_nonce_configs() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-reap-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let bound_port = 9876u16;

        let cfg_body = |port: u16, nonce: &str| {
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:{port}/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{nonce}"}}}}}}}}"#
            )
        };

        // (a) Our port, REGISTERED nonce → keep.
        let wd = format!("D:/reap-live-wt-{}", uuid::Uuid::now_v7());
        let live_nonce = register_proxy_nonce(&wd, None);
        let keep_ours = dir.join("keep-ours.json");
        std::fs::write(&keep_ours, cfg_body(bound_port, &live_nonce)).unwrap();

        // (b) Our port, UNREGISTERED nonce → reap.
        let reap_stale_nonce = dir.join("reap-stale-nonce.json");
        std::fs::write(&reap_stale_nonce, cfg_body(bound_port, "not-registered")).unwrap();

        // (c) Foreign DEAD port → reap. (d) Foreign LIVE port → keep.
        let reap_dead_port = dir.join("reap-dead-port.json");
        std::fs::write(&reap_dead_port, cfg_body(9899, "whatever")).unwrap();
        let keep_live_port = dir.join("keep-live-port.json");
        std::fs::write(&keep_live_port, cfg_body(9877, "sibling")).unwrap();

        // (e) Garbage file → reap.
        let reap_garbage = dir.join("reap-garbage.json");
        std::fs::write(&reap_garbage, "not json {").unwrap();

        // Injected liveness: only :9877 is "alive".
        let reaped = reap_stale_session_restore_configs_in(&dir, bound_port, &|p| p == 9877);

        assert_eq!(reaped, 3, "stale-nonce + dead-port + garbage are reaped");
        assert!(keep_ours.exists(), "our-port registered-nonce config kept");
        assert!(
            keep_live_port.exists(),
            "a live foreign-port config belongs to a sibling runner — kept"
        );
        assert!(!reap_stale_nonce.exists());
        assert!(!reap_dead_port.exists());
        assert!(!reap_garbage.exists());

        // Cleanup.
        revoke_proxy_nonce(&live_nonce);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Session close must reap the config file of EVERY nonce it revoked —
    /// including per-TERMINAL ones, whose filename is derived from the terminal
    /// id that `release_workdir_on_session_close` does not have. This is the
    /// regression guard for the name-keyed reap that per-terminal nonces broke:
    /// a name-based lookup finds only the workdir-derived file and silently
    /// leaves every terminal-keyed sibling on disk.
    #[test]
    fn revoked_nonce_reap_matches_terminal_keyed_configs_not_just_the_workdir_name() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-revreap-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_body = |nonce: &str| {
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{nonce}"}}}}}}}}"#
            )
        };

        let wd = format!("D:/revreap-wt-{}", uuid::Uuid::now_v7());
        // Two terminals in ONE workdir (the case that motivated per-terminal
        // nonces) plus the terminal-less/workdir-derived form.
        let n_t1 = register_proxy_nonce(&wd, Some("terminal-alpha"));
        let n_t2 = register_proxy_nonce(&wd, Some("terminal-beta"));
        let n_none = register_proxy_nonce(&wd, None);
        assert_ne!(n_t1, n_t2, "distinct terminals must hold distinct nonces");

        // Named exactly as production names them — terminal-derived for the two
        // terminals, workdir-derived for the terminal-less one.
        let f_t1 = dir.join(mcp_config_file_name(&wd, Some("terminal-alpha")));
        let f_t2 = dir.join(mcp_config_file_name(&wd, Some("terminal-beta")));
        let f_none = dir.join(mcp_config_file_name(&wd, None));
        std::fs::write(&f_t1, cfg_body(&n_t1)).unwrap();
        std::fs::write(&f_t2, cfg_body(&n_t2)).unwrap();
        std::fs::write(&f_none, cfg_body(&n_none)).unwrap();

        // A bystander config for a DIFFERENT workdir's nonce — never revoked
        // here, so it must survive.
        let other_wd = format!("D:/revreap-other-{}", uuid::Uuid::now_v7());
        let n_other = register_proxy_nonce(&other_wd, Some("terminal-gamma"));
        let f_other = dir.join(mcp_config_file_name(&other_wd, Some("terminal-gamma")));
        std::fs::write(&f_other, cfg_body(&n_other)).unwrap();

        let removed = reap_configs_for_revoked_nonces(&dir, &[n_t1, n_t2, n_none]);

        assert_eq!(removed, 3, "all three revoked-nonce configs reaped");
        assert!(!f_t1.exists(), "terminal-alpha's config reaped");
        assert!(!f_t2.exists(), "terminal-beta's config reaped");
        assert!(!f_none.exists(), "workdir-derived config reaped");
        assert!(
            f_other.exists(),
            "another workdir's live nonce must not be reaped by this close"
        );

        // An empty revoked set is a no-op, not a directory sweep.
        assert_eq!(reap_configs_for_revoked_nonces(&dir, &[]), 0);
        assert!(f_other.exists());

        revoke_proxy_nonce(&n_other);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Phase 2 of plan `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`:
/// the proxy nonce now also travels in `Authorization: Bearer <nonce>`, which
/// is what stops a stale key's 401 escalating into OAuth Dynamic Client
/// Registration (and the durable `mcpOAuth` poison entry a failed DCR leaves
/// behind).
///
/// The emitter change is one line. The tests here exist for the READERS: five
/// of them derived the header name from a hardcoded literal, and every one
/// degraded SILENTLY on the new shape — a `None`, a `continue`, an
/// `unwrap_or("")`. Nothing in the build or the old test suite would have
/// failed.
#[cfg(test)]
mod phase2_proxy_header_shape_tests {
    use super::*;

    /// A JWT-SHAPED string (three non-empty dot-separated segments) whose
    /// payload decodes `sub_type=agent` — the static-bearer agent config's
    /// shape, which every reader must keep treating as NOT a proxy nonce.
    /// `{"sub_type":"agent"}` base64url-no-pad.
    const AGENT_JWT: &str = "eyJhbGciOiJFZERTQSJ9.eyJzdWJfdHlwZSI6ImFnZW50In0.c2ln";

    fn new_shape_config(port: u16, nonce: &str) -> String {
        format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:{port}/coord-mcp","headers":{{"Authorization":"Bearer {nonce}"}}}}}}}}"#
        )
    }

    /// The emitter writes BOTH names. `Authorization` is the load-bearing one
    /// (its presence in the STATIC headers map is what stops the client
    /// attaching an OAuth provider); the legacy custom header stays because the
    /// `qontinui-claude-config` recovery doors — `/gate`, `/policy`,
    /// `/coord-revive`, `/pr-status` — read this file by that name, and
    /// dropping it would blind exactly the tooling used to diagnose this
    /// failure.
    #[test]
    fn proxy_config_emits_both_header_shapes() {
        let nonce = register_proxy_nonce(
            &format!("D:/phase2-emit-{}", uuid::Uuid::now_v7()),
            Some("terminal-emit"),
        );
        let doc = coord_mcp_proxy_config_json(9876, &nonce);
        let headers = &doc["mcpServers"]["coord-mcp"]["headers"];
        assert_eq!(
            headers["Authorization"],
            serde_json::Value::from(format!("Bearer {nonce}")),
            "the OAuth-suppressing header must carry the nonce as a bearer"
        );
        assert_eq!(
            headers["X-Coord-Mcp-Proxy-Key"],
            serde_json::Value::from(nonce.as_str()),
            "the legacy header must keep carrying the raw nonce"
        );
        // No baked JWT: the value in `Authorization` is a nonce, never a token.
        assert!(
            !crate::coord_mcp_config::looks_like_jwt(&nonce),
            "a proxy nonce must never be JWT-shaped — every reader's \
             nonce-vs-bearer discriminator depends on it"
        );
    }

    /// `read_proxy_nonce` must resolve the runner's OWN new-shape output.
    ///
    /// If it did not, `resolve_root_reconcile` would set `on_disk_nonce=None` /
    /// `registered=false`, and `root_reconcile_action` would classify a fresh,
    /// healthy config as a non-proxy shape and LEAVE it — killing the boot
    /// self-heal and the adopt-on-disk path on exactly the configs this phase
    /// produces.
    #[test]
    fn read_proxy_nonce_resolves_every_shape_the_writer_can_produce() {
        let dir = std::env::temp_dir().join(format!("phase2-read-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        let nonce = register_proxy_nonce(&dir.to_string_lossy(), None);

        // (a) The real emitter's output, round-tripped through the real writer.
        write_mcp_json(
            &dir.to_string_lossy(),
            &coord_mcp_proxy_config_json(9876, &nonce),
        );
        assert_eq!(
            read_proxy_nonce(&path).as_deref(),
            Some(nonce.as_str()),
            "the writer's own output must read back"
        );

        // (b) Authorization-only (the shape a later phase may narrow to).
        std::fs::write(&path, new_shape_config(9876, &nonce)).unwrap();
        assert_eq!(read_proxy_nonce(&path).as_deref(), Some(nonce.as_str()));

        // (c) Legacy custom-header-only — every `.mcp.json` written before this
        // phase, which keeps working because configs are rewritten on session
        // spawn, never periodically.
        std::fs::write(
            &path,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{nonce}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(read_proxy_nonce(&path).as_deref(), Some(nonce.as_str()));

        // (d) The static-bearer AGENT shape must still read as NOT-a-proxy —
        // otherwise the reconcile would treat a user/agent config as ours.
        std::fs::write(
            &path,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://coord.example.test/mcp","headers":{{"Authorization":"Bearer {AGENT_JWT}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(
            read_proxy_nonce(&path),
            None,
            "a real bearer is not a proxy nonce"
        );
    }

    /// End-to-end on the self-heal predicate itself: a new-shape config on the
    /// bound port whose nonce is REGISTERED is healthy (`Leave`); the same
    /// config with an unregistered nonce is adopted, not silently ignored.
    #[test]
    fn resolve_root_reconcile_still_self_heals_a_new_shape_config() {
        let dir = std::env::temp_dir().join(format!("phase2-reconcile-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        let bound = 9876u16;

        let live = register_proxy_nonce(&dir.to_string_lossy(), Some("terminal-reconcile"));
        std::fs::write(&path, new_shape_config(bound, &live)).unwrap();
        let (action, seen) = resolve_root_reconcile(&dir, bound);
        assert_eq!(seen.as_deref(), Some(live.as_str()));
        assert_eq!(
            action,
            RootReconcileAction::Leave,
            "a registered nonce on the bound port is healthy"
        );

        // An UNREGISTERED nonce in the new shape must still be adopted — the
        // regression that would otherwise hide here is `Leave`, because an
        // unreadable nonce classifies as a non-proxy shape.
        let orphan = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        std::fs::write(&path, new_shape_config(bound, &orphan)).unwrap();
        let (action, seen) = resolve_root_reconcile(&dir, bound);
        assert_eq!(seen.as_deref(), Some(orphan.as_str()));
        assert_eq!(
            action,
            RootReconcileAction::AdoptNonce,
            "an unregistered new-shape nonce must be ADOPTED, not left alone"
        );
    }

    /// The revoked-config reaper is nonce-keyed via `read_proxy_nonce`, so a
    /// new-shape config must still be matched. A miss here is silent (`let
    /// Some(..) else { continue }`) and leaves dead credential files on disk.
    #[test]
    fn revoked_config_reaper_matches_a_new_shape_config() {
        let dir = std::env::temp_dir().join(format!("phase2-reap-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();

        let revoked_new = format!("{}", uuid::Uuid::new_v4().simple()).repeat(2);
        let revoked_legacy = format!("{}", uuid::Uuid::new_v4().simple()).repeat(2);
        let survivor = format!("{}", uuid::Uuid::new_v4().simple()).repeat(2);

        let f_new = dir.join("new-shape.json");
        let f_legacy = dir.join("legacy-shape.json");
        let f_keep = dir.join("survivor.json");
        std::fs::write(&f_new, new_shape_config(9876, &revoked_new)).unwrap();
        std::fs::write(
            &f_legacy,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{revoked_legacy}"}}}}}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(&f_keep, new_shape_config(9876, &survivor)).unwrap();

        let removed =
            reap_configs_for_revoked_nonces(&dir, &[revoked_new.clone(), revoked_legacy.clone()]);
        assert_eq!(removed, 2, "both shapes must be reaped");
        assert!(!f_new.exists(), "the new-shape config must be reaped");
        assert!(!f_legacy.exists(), "the legacy config must still be reaped");
        assert!(f_keep.exists(), "an unrevoked nonce's config is untouched");
    }

    /// A rotation `write` line must carry a NON-EMPTY `key_prefix` for a
    /// new-shape config. The old extraction was `pointer(...legacy header...)
    /// .unwrap_or("")`, which would have logged an empty prefix on every single
    /// write — silently destroying the mint→write→evict join that made the
    /// 2026-08-19 incident reconstructible at all.
    #[test]
    fn rotation_write_line_carries_a_non_empty_key_prefix_for_the_new_shape() {
        let log_dir = rotation_log_test_dir();
        let wt = std::env::temp_dir().join(format!("phase2-write-line-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&wt).unwrap();
        let wt_str = wt.to_string_lossy().to_string();

        let nonce = register_proxy_nonce(&wt_str, Some("terminal-write-line"));
        write_mcp_json(&wt_str, &coord_mcp_proxy_config_json(9876, &nonce));

        let raw = std::fs::read_to_string(log_dir.join(ROTATION_LOG_FILE)).unwrap();
        let writes: Vec<serde_json::Value> = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["workdir"] == wt_str.as_str() && v["event"] == "write")
            .collect();
        assert_eq!(writes.len(), 1, "one write line for this workdir");
        let prefix = writes[0]["key_prefix"].as_str().unwrap_or("");
        assert!(
            !prefix.is_empty(),
            "the write line's key_prefix must not be empty for a new-shape config"
        );
        assert_eq!(
            prefix,
            &nonce[..ROTATION_KEY_PREFIX_LEN],
            "and it must be THIS write's key, so the mint→write join still holds"
        );
    }

    /// `coord_mcp_safe_to_write`'s agent-JWT guard used to rest on the proxy
    /// shape having NO `Authorization` header at all. It has one now, so the
    /// guard rests instead on the VALUE's shape: a 64-hex nonce fails the JWT
    /// decode, a real agent token does not. Pinned here because that is the
    /// only thing standing between "refresh our own config" and "clobber a
    /// session's richer agent credential".
    #[test]
    fn safe_to_write_keeps_a_nonce_bearing_proxy_config_refreshable() {
        let dir = std::env::temp_dir().join(format!("phase2-safe-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");
        let nonce = register_proxy_nonce(&wd, Some("terminal-safe"));

        // The real emitter's output — Authorization present, value a nonce.
        std::fs::write(
            &mcp,
            serde_json::to_string_pretty(&coord_mcp_proxy_config_json(9876, &nonce)).unwrap(),
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "a proxy config whose Authorization carries a NONCE is ours — refreshable"
        );

        // A real agent JWT in the same slot is still refused (never downgrade
        // an agent credential to a device one).
        std::fs::write(
            &mcp,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://coord.example.test/mcp","headers":{{"Authorization":"Bearer {AGENT_JWT}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd, IntendedWrite::Device),
            "an agent JWT must still block the write"
        );
    }

    /// The 2am string. The old one was literally
    /// `"missing or unrecognized X-Coord-Mcp-Proxy-Key"` — it named the header
    /// this phase deprecates and said nothing about what to do. The replacement
    /// must name the cause, say why reconnecting cannot work, and give a
    /// concrete recovery.
    ///
    /// **This test used to assert `provision-session`** and so PINNED an
    /// instruction that does not work: that route is gated by the same-user
    /// loopback handshake AND the opt-in marker (an un-opted machine ⇒ every
    /// request denied) and, even opted in, hands back an ephemeral terminal-less
    /// nonce — a weaker class than the one that just died. The assertions below
    /// now pin the three Phase-5-VERIFIED steps instead, plus a negative that
    /// keeps the falsified route from creeping back in.
    #[test]
    fn the_dead_key_error_string_is_actionable_and_names_no_deprecated_header_alone() {
        let msg = stale_proxy_key_error(STALE_PROXY_KEY_CAUSE);
        assert!(msg.contains("stale or unrecognized coord-mcp proxy key"));
        assert!(
            msg.contains("never re-reads"),
            "must say why reconnecting the server cannot recover it"
        );
        // The three verified steps, in order.
        assert!(
            msg.contains("/coord-revive"),
            "step 1: keep working through another coord door"
        );
        assert!(
            msg.contains("start a NEW session"),
            "step 2: the runner writes a fresh key on every session spawn"
        );
        assert!(
            msg.contains("claude mcp logout"),
            "step 3: the VERIFIED credential-store repair for a poisoned client"
        );
        assert!(
            msg.contains("NEVER restart the runner"),
            "the one recovery an operator must not reach for"
        );
        // Negative: the flag-gated, ephemeral-credential route must NOT be
        // advertised as a recovery. An unverified (here: falsified) recovery
        // instruction is the defect class this plan retires.
        assert!(
            !msg.contains("provision-session"),
            "the default-OFF, ephemeral-nonce route is not a recovery"
        );
        // Both accepted header names appear, so a reader is never pointed at
        // one name as if it were the only one.
        assert!(msg.contains("Authorization: Bearer"));
        assert!(msg.contains(COORD_MCP_PROXY_KEY_HEADER_JSON));
        // Every door's message shares the recovery tail — five doors, one story.
        for cause in [
            STALE_PROXY_KEY_CAUSE,
            NON_DEVICE_PROXY_KEY_CAUSE,
            AGENT_GONE_PROXY_CAUSE,
        ] {
            assert!(stale_proxy_key_error(cause).contains(PROXY_KEY_RECOVERY_HINT));
        }
        // These strings are WIRE text, not source layout. Each was written as a
        // wrapped multi-line literal and every one of them shipped with the line
        // indentation baked into the value — the 401 body literally read
        // `"...loopback nonce is      not registered..."`. A `\`-continued
        // literal is the only shape that wraps in source without wrapping in the
        // value, and this assertion is what keeps a later re-wrap honest.
        for s in [
            STALE_PROXY_KEY_CAUSE,
            NON_DEVICE_PROXY_KEY_CAUSE,
            AGENT_GONE_PROXY_CAUSE,
            PROXY_KEY_RECOVERY_HINT,
        ] {
            assert!(
                !s.contains("  "),
                "operator-facing text must carry no run of spaces from source \
                 wrapping, got: {s:?}"
            );
            assert!(!s.contains('\n'), "one line, no embedded newlines: {s:?}");
        }
        // And the gate itself now returns it rather than the old header name.
        let (status, gate_msg) = proxy_request_gate(None, None, &ProxyPrincipal::Device)
            .expect_err("an absent nonce must be rejected");
        assert_eq!(status, 401);
        assert_eq!(gate_msg, msg);
    }
}

#[cfg(test)]
mod agent_binding_census_tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Agent-binding census + boot liveness readback (plan
    // `2026-08-25-agent-class-sessions-reach-coord-like-operator-sessions`
    // Phase 1). These pin the ONE property the phase exists for: that the two
    // readings left open by the vet — "agent sessions die with the runner" vs
    // "they survive and go quiet" — land in DIFFERENT buckets, and that an
    // absent signal lands in neither.
    // -----------------------------------------------------------------------

    fn census_test_binding(
        principal: ProxyPrincipal,
        workdir: &str,
        terminal_id: Option<&str>,
    ) -> NonceBinding {
        NonceBinding {
            workdir: workdir.to_string(),
            principal,
            lifetime: NonceLifetime::Persistent,
            session_pin: crate::session::tenant_pin::TenantPin::Unpinned,
            terminal_id: terminal_id.map(str::to_owned),
            minted_at: std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_700_000_000),
        }
    }

    /// The census selects EXACTLY what `device_nonce_snapshot` discards — the
    /// agent bindings — and nothing else. This is the selection half of Phase 1:
    /// a census that also caught device bindings would report a population that
    /// is not the one OQ3 is about.
    #[test]
    fn census_selects_exactly_the_bindings_device_snapshot_discards() {
        let agent_a = uuid::Uuid::parse_str("00000000-0000-7000-8000-00000000000a").unwrap();
        let agent_b = uuid::Uuid::parse_str("00000000-0000-7000-8000-00000000000b").unwrap();
        let mut map = HashMap::new();
        map.insert(
            "n-device".to_string(),
            census_test_binding(ProxyPrincipal::Device, "D:/wt/device", Some("term-1")),
        );
        map.insert(
            "n-agent-b".to_string(),
            census_test_binding(ProxyPrincipal::Agent { agent_id: agent_b }, "D:/wt/b", None),
        );
        map.insert(
            "n-agent-a".to_string(),
            census_test_binding(ProxyPrincipal::Agent { agent_id: agent_a }, "D:/wt/a", None),
        );

        let census = agent_binding_census(&map);
        assert_eq!(census.len(), 2, "device bindings are not agent bindings");
        assert_eq!(
            census.iter().map(|e| e.agent_id).collect::<Vec<_>>(),
            vec![agent_a, agent_b],
            "the census is deterministically ordered — the emit-on-change gate \
             compares consecutive censuses and a shuffled order would emit on \
             every mint"
        );
        assert_eq!(census[0].workdir, "D:/wt/a");
        assert_eq!(
            census[0].terminal_id, None,
            "an agent binding has no terminal BY CONSTRUCTION — the census must \
             carry that as absence, not fabricate one"
        );
        assert_eq!(census[0].minted_at_unix, 1_700_000_000);

        // The complement is the persisted set: the two projections must not
        // overlap, or the census would be counting something that DOES reach
        // disk.
        let persisted = device_nonce_snapshot(&map);
        assert_eq!(persisted.len(), 1);
        assert!(persisted.contains_key("n-device"));
    }

    /// The gate: FIRST census of a process always emits — empty or not — then
    /// only on change. A silently absent census line is indistinguishable from
    /// a healthy zero, and this fleet's expected steady state IS zero, so the
    /// zero has to be stated out loud at every boot
    /// (`verification-and-evidence` `silent-empty-is-unknown`).
    #[test]
    fn first_census_emits_even_when_empty_then_only_on_change() {
        let mut prev: Option<Vec<AgentBindingCensusEntry>> = None;
        assert!(
            census_should_emit(&mut prev, &[]),
            "a boot must state its zero, not stay silent"
        );
        assert!(
            !census_should_emit(&mut prev, &[]),
            "an unchanged census does not re-emit — this hangs off the mint path"
        );

        let entry = AgentBindingCensusEntry {
            agent_id: uuid::Uuid::now_v7(),
            workdir: "D:/wt/x".to_string(),
            terminal_id: None,
            minted_at_unix: 42,
        };
        assert!(census_should_emit(&mut prev, std::slice::from_ref(&entry)));
        assert!(!census_should_emit(&mut prev, std::slice::from_ref(&entry)));
        assert!(
            census_should_emit(&mut prev, &[]),
            "dropping back to zero is a change, and is the line that says a \
             population went away rather than the log going quiet"
        );
    }

    /// The emitted JSON shape, end to end through the real rotation writer: an
    /// agent mint must leave a census line naming that agent, its workdir, its
    /// (absent) terminal and its mint time.
    #[test]
    fn agent_mint_emits_a_census_line_with_the_binding_fields() {
        let dir = rotation_log_test_dir();
        let agent_id = uuid::Uuid::now_v7();
        let wd = format!("D:/census-wt-{}", uuid::Uuid::now_v7());
        let _nonce = register_agent_proxy_nonce(&wd, agent_id);

        let raw = std::fs::read_to_string(dir.join(ROTATION_LOG_FILE)).unwrap();
        let row = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "agent_binding_census")
            .filter_map(|v| {
                v["bindings"]
                    .as_array()?
                    .iter()
                    .find(|b| b["agent_id"] == agent_id.to_string().as_str())
                    .cloned()
            })
            .next()
            .expect("a census line naming the agent just minted for");

        assert_eq!(row["workdir"], wd.as_str());
        assert_eq!(
            row["terminal_id"],
            serde_json::Value::Null,
            "null, never the string \"unknown\" — an agent binding HAS no \
             terminal, which is a different fact from one the runner failed to \
             record"
        );
        assert!(row["minted_at_unix"].as_u64().unwrap_or(0) > 0);

        // The line is aggregate: no key material, and the count is present.
        let line = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "agent_binding_census")
            .find(|v| {
                v["bindings"].as_array().is_some_and(|b| {
                    b.iter()
                        .any(|r| r["agent_id"] == agent_id.to_string().as_str())
                })
            })
            .unwrap();
        assert_eq!(line["workdir"], ROTATION_UNKNOWN, "aggregate line");
        assert_eq!(
            line["key_prefix"],
            rotation_key_prefix(ROTATION_UNKNOWN).as_str()
        );
        assert!(line["agent_bindings"].as_u64().unwrap_or(0) >= 1);
        assert!(
            !raw.contains(&_nonce),
            "a census line never carries a nonce"
        );
    }

    /// A mid-file tail read starts inside a line; that fragment is dropped, not
    /// fed to the parser as if it were a record.
    #[test]
    fn tail_read_drops_the_partial_first_line() {
        assert_eq!(
            drop_partial_first_line("{\"a\":1}\n{\"b\":2}\n", false),
            "{\"a\":1}\n{\"b\":2}\n"
        );
        assert_eq!(
            drop_partial_first_line("a\":1}\n{\"b\":2}\n", true),
            "{\"b\":2}\n"
        );
        assert_eq!(drop_partial_first_line("no newline at all", true), "");
    }

    fn census_fixture_line(ts: &str, pid: u32, bindings: &str) -> String {
        format!(
            "{{\"ts\":\"{ts}\",\"event\":\"agent_binding_census\",\"workdir\":\"unknown\",\
             \"runner_id\":\"primary\",\"pid\":{pid},\"agent_bindings\":1,\"bindings\":[{bindings}]}}"
        )
    }

    /// The readback takes the NEWEST census, tolerates torn/foreign lines, and
    /// decodes the binding rows.
    #[test]
    fn readback_takes_the_newest_census_and_survives_torn_lines() {
        let old = census_fixture_line(
            "2026-08-20T10:00:00+00:00",
            111,
            "{\"agent_id\":\"00000000-0000-7000-8000-00000000000a\",\"workdir\":\"D:/old\",\"terminal_id\":null,\"minted_at_unix\":1}",
        );
        let new = census_fixture_line(
            "2026-08-24T16:00:00+00:00",
            222,
            "{\"agent_id\":\"00000000-0000-7000-8000-00000000000b\",\"workdir\":\"D:/new\",\"terminal_id\":\"term-9\",\"minted_at_unix\":2}",
        );
        let tail = format!(
            "{old}\n{{not json at all\n{{\"event\":\"mint\"}}\n{new}\n{{\"event\":\"reject\"}}\n"
        );

        let c = parse_last_agent_binding_census(&tail, 0).expect("a census in the tail");
        assert_eq!(c.pid, 222, "the newest census wins");
        assert_eq!(c.runner_id, "primary");
        assert_eq!(c.ts_unix, Some(1_787_587_200), "2026-08-24T16:00:00Z");
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.entries[0].workdir, "D:/new");
        assert_eq!(c.entries[0].terminal_id.as_deref(), Some("term-9"));

        // Our OWN census is skipped: the boot task runs seconds in, so a
        // restored terminal may already have written one, and reading it would
        // root the survivor probe at a pid that is trivially alive.
        let c = parse_last_agent_binding_census(&tail, 222).expect("the older census");
        assert_eq!(
            c.pid, 111,
            "a census this process wrote is not the predecessor's"
        );

        assert!(
            parse_last_agent_binding_census("{\"event\":\"mint\"}\n", 0).is_none(),
            "no census line is None — the caller renders that as UNKNOWN, not zero"
        );
    }

    /// Build a synthetic process table: `parent_map` edges plus per-pid image
    /// names and creation times. Nothing here touches a real process.
    fn census_snapshot(
        edges: &[(u32, u32)],
        claude_pids: &[u32],
        created: &[(u32, i64)],
    ) -> crate::process_capture::process_tree::ProcessSnapshot {
        let mut snap = crate::process_capture::process_tree::ProcessSnapshot::default();
        for (parent, child) in edges {
            snap.parent_map.entry(*parent).or_default().push(*child);
        }
        for pid in claude_pids {
            snap.names.insert(*pid, "claude.exe".to_string());
        }
        for (pid, t) in created {
            snap.creation_times.insert(*pid, *t);
        }
        snap
    }

    fn census_with(
        pid: u32,
        ts_unix: i64,
        entries: Vec<AgentBindingCensusEntry>,
    ) -> LastAgentCensus {
        LastAgentCensus {
            ts: "2026-08-24T16:00:00+00:00".to_string(),
            ts_unix: Some(ts_unix),
            runner_id: "primary".to_string(),
            pid,
            declared_bindings: Some(entries.len() as u64),
            rows_present: true,
            entries,
        }
    }

    fn census_entry(terminal_id: Option<&str>) -> AgentBindingCensusEntry {
        AgentBindingCensusEntry {
            agent_id: uuid::Uuid::now_v7(),
            workdir: "D:/wt/agent".to_string(),
            terminal_id: terminal_id.map(str::to_owned),
            minted_at_unix: 1000,
        }
    }

    /// **The (i)-vs-(ii) discriminator.** Reading (i) — the previous runner's
    /// whole claude subtree died with it — is the ONLY thing that yields `dead`.
    #[test]
    fn nothing_survived_the_previous_runner_reads_as_dead() {
        // pid 100 was the previous runner; its one child (200, non-claude) is
        // all that is left, so no claude outlived it.
        let snap = census_snapshot(&[(1, 100), (100, 200)], &[], &[(200, 500)]);
        let census = census_with(100, 1000, vec![census_entry(None), census_entry(None)]);

        let t = classify_agent_binding_liveness(&census, &snap, &HashMap::new(), 2_000_000);
        assert_eq!(t.survivors, 0);
        assert_eq!((t.alive, t.dead, t.unknown), (0, 2, 0));
        assert_eq!(t.agent_bindings, 2);
    }

    /// Reading (ii) — a claude process outlived the runner — must NOT read as
    /// `dead`. It cannot read as `alive` either while agent bindings carry no
    /// terminal to join on, so it lands in `unknown`. That third bucket is the
    /// whole point: collapsing it into `dead` would have re-created the
    /// unwatched premise this instrumentation exists to watch.
    #[test]
    fn a_survivor_the_census_cannot_attribute_is_unknown_not_dead() {
        let snap = census_snapshot(&[(1, 100), (100, 200)], &[200], &[(200, 500)]);
        let census = census_with(100, 1000, vec![census_entry(None)]);

        let t = classify_agent_binding_liveness(&census, &snap, &HashMap::new(), 2_000_000);
        assert_eq!(t.survivors, 1, "a claude child outlived the dead runner");
        assert_eq!((t.alive, t.dead, t.unknown), (0, 0, 1));
    }

    /// A binding that DOES carry a terminal, whose PTY hosts a live claude this
    /// boot, is attributable — and is the only shape that yields `alive`.
    #[test]
    fn a_terminal_bearing_binding_hosting_a_live_claude_is_alive() {
        // 300 is this boot's PTY child; 301 is a live claude under it.
        let snap = census_snapshot(
            &[(1, 100), (100, 200), (1, 300), (300, 301)],
            &[200, 301],
            &[(200, 500), (300, 1_500), (301, 1_600)],
        );
        let census = census_with(100, 1000, vec![census_entry(Some("term-live"))]);
        let terminals = HashMap::from([("term-live".to_string(), 300u32)]);

        let t = classify_agent_binding_liveness(&census, &snap, &terminals, 1_000_000);
        assert_eq!((t.alive, t.dead, t.unknown), (1, 0, 0));
        assert_eq!(t.signal, "prev_runner_subtree+terminal_join");
    }

    /// Every arm where the ORACLE could not run reports UNKNOWN for every
    /// binding — never `dead`, and never a silent zero. An unreadable process
    /// table, an unparseable census timestamp, a missing pid, and a RECYCLED
    /// pid are four different ways to know nothing, and all four must refuse to
    /// answer (`verification-and-evidence` `silent-empty-is-unknown`).
    #[test]
    fn an_unrunnable_oracle_is_unknown_never_dead() {
        let entries = vec![census_entry(None), census_entry(None), census_entry(None)];

        // 1 — empty process table: "could not see", not "nothing there".
        let empty = crate::process_capture::process_tree::ProcessSnapshot::default();
        let t = classify_agent_binding_liveness(
            &census_with(100, 1000, entries.clone()),
            &empty,
            &HashMap::new(),
            0,
        );
        assert_eq!((t.alive, t.dead, t.unknown), (0, 0, 3));
        assert_eq!(t.signal, "process_table_unavailable");

        let snap = census_snapshot(&[(1, 100)], &[], &[]);

        // 2 — unparseable census timestamp: the PID-reuse guard is gone.
        let mut bad_ts = census_with(100, 1000, entries.clone());
        bad_ts.ts_unix = None;
        let t = classify_agent_binding_liveness(&bad_ts, &snap, &HashMap::new(), 0);
        assert_eq!(t.unknown, 3);
        assert_eq!(t.signal, "census_timestamp_unparseable");

        // 3 — no emitting pid on the census line: nothing to root the probe at.
        let t = classify_agent_binding_liveness(
            &census_with(0, 1000, entries.clone()),
            &snap,
            &HashMap::new(),
            0,
        );
        assert_eq!(t.unknown, 3);
        assert_eq!(t.signal, "census_pid_absent");

        // 4 — the previous runner's pid now belongs to a process created AFTER
        // the census: its subtree is a stranger's, and counting it would
        // manufacture survivors.
        let recycled = census_snapshot(
            &[(1, 100), (100, 200)],
            &[200],
            &[(100, 5_000), (200, 5_100)],
        );
        let t = classify_agent_binding_liveness(
            &census_with(100, 1000, entries),
            &recycled,
            &HashMap::new(),
            0,
        );
        assert_eq!((t.alive, t.dead, t.unknown), (0, 0, 3));
        assert_eq!(t.signal, "prev_runner_pid_recycled");
    }

    /// **F1 regression — the bug that made the detector unable to ever fire.**
    ///
    /// The census is written at nonce-MINT time (`agent_runtime.rs:3900`); the
    /// claude child is spawned afterwards (`:3978`), behind an HTTP probe and,
    /// on the respawn arm, a whole prior run. So a genuine survivor is created
    /// AFTER the census, not before. Filtering survivors on the census ts
    /// excluded every one of them: `survivors` was always empty, `terminal_id`
    /// is always `None`, and so every binding fell to `dead` — the line read
    /// `{"alive":0,"dead":1,"survivors":0}` for a session running right now.
    ///
    /// The reference is THIS BOOT. A process created after the census but
    /// before the restart is exactly what "outlived the runner" means.
    #[test]
    fn a_survivor_created_after_the_census_is_still_a_survivor() {
        // Census at t=1000. Claude child created at t=1500 (after the census,
        // as production always does). Restart/boot at t=3000.
        let snap = census_snapshot(&[(1, 100), (100, 200)], &[200], &[(200, 1_500)]);
        let census = census_with(100, 1000, vec![census_entry(None)]);

        let t = classify_agent_binding_liveness(&census, &snap, &HashMap::new(), 3_000_000);
        assert_eq!(
            t.survivors, 1,
            "a claude spawned after its own census line is the NORMAL case — \
             keying the filter on the census ts made every real survivor invisible"
        );
        assert_eq!(
            (t.alive, t.dead, t.unknown),
            (0, 0, 1),
            "reading (ii) must never be reported as `dead`"
        );
    }

    /// A process created after THIS BOOT did not outlive anything — it is one of
    /// the new runner's own children and must not be counted as a survivor.
    #[test]
    fn a_process_created_after_this_boot_is_not_a_survivor() {
        // Boot at t=3000; the claude at 4000 postdates it.
        let snap = census_snapshot(&[(1, 100), (100, 200)], &[200], &[(200, 4_000)]);
        let census = census_with(100, 1000, vec![census_entry(None)]);

        let t = classify_agent_binding_liveness(&census, &snap, &HashMap::new(), 3_000_000);
        assert_eq!(t.survivors, 0);
        assert_eq!((t.alive, t.dead, t.unknown), (0, 1, 0));
    }

    /// **F3 — the census's runner must actually be gone.** Two runner processes
    /// can share one instance's log dir (an overlapping shutdown, or a second
    /// launch of the same instance). The peer's pid is LIVE and predates its own
    /// census, so the recycle guard does not fire — and classifying would report
    /// every one of a live peer's bindings as `dead`.
    #[test]
    fn a_live_census_writer_is_refused_not_classified() {
        // pid 100 is in the process table, created at t=500, i.e. before the
        // census it wrote at t=1000. It never died.
        let snap = census_snapshot(&[(1, 100), (100, 200)], &[200], &[(100, 500), (200, 1_500)]);
        let census = census_with(100, 1000, vec![census_entry(None), census_entry(None)]);

        let t = classify_agent_binding_liveness(&census, &snap, &HashMap::new(), 3_000_000);
        assert_eq!(
            (t.alive, t.dead, t.unknown),
            (0, 0, 2),
            "a live peer's bindings are UNKNOWN — reporting them dead is the \
             same false negative F1 produced, by a different route"
        );
        assert_eq!(t.signal, "prev_runner_still_alive");
    }

    /// **F2 — a census whose rows did not decode is UNKNOWN, never a zero.**
    ///
    /// The line carries its own authoritative `agent_bindings` count. When the
    /// decoded rows disagree — one malformed `agent_id`, or a `bindings` key
    /// that is missing or not an array — the readback must refuse. Otherwise a
    /// schema change reports `agent_bindings: 0` for a boot that stranded
    /// three, byte-identical to this fleet's healthy steady state.
    #[test]
    fn a_partially_decoded_census_is_unknown_not_a_healthy_zero() {
        let snap = census_snapshot(&[(1, 100)], &[], &[]);

        // Declared 3, only 1 row survived the decode.
        let mut partial = census_with(100, 1000, vec![census_entry(None)]);
        partial.declared_bindings = Some(3);
        assert!(!partial.rows_decodable());
        let t = classify_agent_binding_liveness(&partial, &snap, &HashMap::new(), 3_000_000);
        assert_eq!(t.signal, "census_rows_undecodable");
        assert_eq!(
            (t.agent_bindings, t.alive, t.dead, t.unknown),
            (3, 0, 0, 3),
            "the DECLARED count is reported, not the count that happened to decode"
        );

        // `bindings` absent or not an array: also zero rows, also not a zero.
        let mut no_rows = census_with(100, 1000, vec![]);
        no_rows.rows_present = false;
        assert!(!no_rows.rows_decodable());
        let t = classify_agent_binding_liveness(&no_rows, &snap, &HashMap::new(), 3_000_000);
        assert_eq!(t.signal, "census_rows_undecodable");

        // And the honest zero still reads as a zero.
        let empty = census_with(100, 1000, vec![]);
        assert!(empty.rows_decodable());
    }

    /// The parser carries BOTH halves of the decode — the count the line
    /// declared and whether `bindings` was an array — so the integrity check
    /// above has something to check against.
    #[test]
    fn the_parser_records_the_declared_count_and_row_presence() {
        let good = "{\"ts\":\"2026-08-24T16:00:00+00:00\",\"event\":\"agent_binding_census\",\
                    \"runner_id\":\"primary\",\"pid\":222,\"agent_bindings\":2,\"bindings\":[\
                    {\"agent_id\":\"00000000-0000-7000-8000-00000000000a\",\"workdir\":\"D:/a\",\
                    \"terminal_id\":null,\"minted_at_unix\":1},\
                    {\"agent_id\":\"not-a-uuid\",\"workdir\":\"D:/b\",\
                    \"terminal_id\":null,\"minted_at_unix\":2}]}";
        let c = parse_last_agent_binding_census(good, 0).expect("a census");
        assert_eq!(c.declared_bindings, Some(2));
        assert!(c.rows_present);
        assert_eq!(c.entries.len(), 1, "the malformed uuid row does not decode");
        assert_eq!(c.declared_len(), 2);
        assert!(
            !c.rows_decodable(),
            "one undecodable row poisons the whole census rather than shrinking it"
        );

        let no_bindings_key = "{\"ts\":\"2026-08-24T16:00:00+00:00\",\
                               \"event\":\"agent_binding_census\",\"pid\":222}";
        let c = parse_last_agent_binding_census(no_bindings_key, 0).expect("a census");
        assert!(
            !c.rows_present,
            "an absent `bindings` key is not an empty set"
        );
        assert!(!c.rows_decodable());
    }

    /// **F4 — an ABSENT log is "nothing to read"; an unreadable one is not.**
    /// The absent case flows into the no-census branch, which announces
    /// `census_found:false`; the unreadable case gets its own named signal,
    /// because a runner that can write but says nothing looks exactly like a
    /// build without this instrumentation.
    #[test]
    fn an_absent_log_reads_empty_but_an_unreadable_one_is_named() {
        let dir = std::env::temp_dir().join(format!("rot-tail-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();

        // Absent file → readable-and-empty, NOT a failure.
        match read_rotation_log_tail_at(&dir.join("nope.jsonl")) {
            RotationTail::Text(t) => assert!(t.is_empty()),
            _ => panic!("an absent rotation log is 'nothing to read', not a read failure"),
        }

        // A real file round-trips.
        let f = dir.join(ROTATION_LOG_FILE);
        std::fs::write(&f, "{\"event\":\"mint\"}\n").unwrap();
        match read_rotation_log_tail_at(&f) {
            RotationTail::Text(t) => assert!(t.contains("mint")),
            _ => panic!("a readable log must come back as text"),
        }

        // A path that is a DIRECTORY cannot be read as a log, and that is a
        // failure to READ — distinct from there being nothing there.
        match read_rotation_log_tail_at(&dir) {
            RotationTail::Unreadable(why) => assert!(!why.is_empty(), "the reason is named"),
            _ => panic!("an unreadable path must not masquerade as an empty log"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Phase 3 of plan `2026-08-31-coord-mcp-credential-selection-by-binding-provenance`
/// — the proxy reads its own upstream failures.
///
/// These tests exist for one reason: the two failure classes this phase
/// separates were **byte-indistinguishable** before it, and a regression would
/// be silent again. Nothing in the build, and nothing in the old suite, failed
/// while every class-B session was being told it held a stale proxy key.
#[cfg(test)]
mod proxy_failure_layer_tests {
    use super::*;

    /// 3b's core claim. If these three ever collide, `/coord-revive` and every
    /// other consumer is back to guessing from a bare 401.
    #[test]
    fn the_three_layers_have_distinct_stable_tokens() {
        let tokens = [
            ProxyFailureLayer::RunnerNonce.as_str(),
            ProxyFailureLayer::CoordUpstream.as_str(),
            ProxyFailureLayer::RunnerTransport.as_str(),
        ];
        let mut seen = std::collections::HashSet::new();
        for t in tokens {
            assert!(!t.is_empty(), "a layer token must never be empty");
            assert!(seen.insert(t), "duplicate layer token: {t}");
        }
        // Pinned, not merely distinct: these strings are the wire contract the
        // agent-side doors match on, so a rename is a breaking change and must
        // fail here rather than in the field.
        assert_eq!(ProxyFailureLayer::RunnerNonce.as_str(), "runner-nonce");
        assert_eq!(ProxyFailureLayer::CoordUpstream.as_str(), "coord-upstream");
        assert_eq!(
            ProxyFailureLayer::RunnerTransport.as_str(),
            "runner-transport"
        );
    }

    /// The `next_door` for the two credential layers must give OPPOSITE advice.
    /// This is the actual defect: "start a new session" is correct for a dead
    /// nonce and useless for a dead upstream bearer, because the bearer is
    /// device-wide and follows the session into the new one.
    #[test]
    fn next_door_advice_is_opposite_for_the_two_credential_layers() {
        let nonce = ProxyFailureLayer::RunnerNonce.next_door();
        let upstream = ProxyFailureLayer::CoordUpstream.next_door();
        assert_ne!(nonce, upstream);
        assert!(
            nonce.contains("NEW session"),
            "the runner-nonce recovery IS a new session: {nonce}"
        );
        assert!(
            upstream.contains("will NOT help"),
            "the coord-upstream door must say a new session does not help: {upstream}"
        );
        // Regression guard for a self-review finding: this constant used to
        // assert "the runner has kicked the device-JWT refresher", which is
        // FALSE for an AGENT principal — the proxy never kicks the device
        // refresher for one. Whether a retry happened is measured per-response
        // in `retry`; a constant must not claim it.
        assert!(
            !upstream.contains("has kicked"),
            "next_door is a CONSTANT and must not assert a per-request action: {upstream}"
        );
        // Never advise the one action fleet policy forbids outright
        // (`production-and-cost` `runner-lifecycle`).
        for door in [
            nonce,
            upstream,
            ProxyFailureLayer::RunnerTransport.next_door(),
        ] {
            assert!(
                !door.to_ascii_lowercase().contains("restart the runner"),
                "a recovery hint must never advise restarting the runner: {door}"
            );
        }
    }

    /// A transport failure is UNKNOWN about both credentials. Its advice must
    /// not send anyone to a credential door — that is
    /// `verification-and-evidence` `unknown-must-not-render-as-a-default`
    /// applied to a recovery hint.
    #[test]
    fn transport_layer_implicates_no_credential() {
        let door = ProxyFailureLayer::RunnerTransport.next_door();
        assert!(
            door.contains("Neither credential is implicated"),
            "transport failures must not be reported as a rejection: {door}"
        );
    }

    /// The envelope is ADDITIVE. Every consumer still matching the old prose or
    /// the old `code` keeps working; only the new fields are new.
    #[test]
    fn envelope_preserves_error_and_code_verbatim_and_adds_the_typed_fields() {
        let v = proxy_failure_envelope(
            "the original prose, unchanged",
            "COORD_MCP_PROXY_UNAUTHORIZED",
            ProxyFailureLayer::RunnerNonce,
            "the cause",
            &[("extra", serde_json::Value::from(7))],
        );
        assert_eq!(v["success"], serde_json::Value::Bool(false));
        assert_eq!(v["error"], "the original prose, unchanged");
        assert_eq!(v["code"], "COORD_MCP_PROXY_UNAUTHORIZED");
        assert_eq!(v["layer"], "runner-nonce");
        assert_eq!(v["cause"], "the cause");
        assert_eq!(v["next_door"], ProxyFailureLayer::RunnerNonce.next_door());
        assert_eq!(v["extra"], 7);
        // `probed_at` is the wall-clock of THIS hop — the field that stops a
        // durable artifact asserting unavailability without saying when it
        // learned that (dossier `c632da1c`).
        let probed = v["probed_at"].as_str().expect("probed_at must be a string");
        assert!(
            chrono::DateTime::parse_from_rfc3339(probed).is_ok(),
            "probed_at must be RFC3339, got {probed}"
        );
    }

    /// An `extra` key may override a base key deliberately, but must never
    /// silently drop the typed fields.
    #[test]
    fn envelope_keeps_the_typed_fields_when_extras_are_supplied() {
        let v = proxy_failure_envelope(
            "e",
            "C",
            ProxyFailureLayer::CoordUpstream,
            "c",
            &[
                ("upstreamStatus", serde_json::Value::from(401)),
                ("upstream_body", serde_json::json!({"error": "nope"})),
            ],
        );
        assert_eq!(v["layer"], "coord-upstream");
        assert_eq!(v["upstreamStatus"], 401);
        assert_eq!(v["upstream_body"]["error"], "nope");
        assert!(v.get("next_door").is_some());
        assert!(v.get("probed_at").is_some());
    }
}

/// Phase 3c — one sentinel, not two.
#[cfg(test)]
mod reject_row_workdir_sentinel_tests {
    use super::*;

    /// The measured defect: of 1,049 `reject` rows on the operator box
    /// 2026-08-31, **849 carried `""` and 200 carried `"unknown"`**. A reader
    /// filtering the honest sentinel missed four-fifths of the unattributable
    /// rows. Both must now normalize to the SAME value, or the fix looks
    /// complete while most rows stay invisible.
    #[test]
    fn both_measured_sentinels_normalize_to_one() {
        assert_eq!(normalize_binding_workdir(""), ROTATION_UNKNOWN);
        assert_eq!(normalize_binding_workdir("unknown"), ROTATION_UNKNOWN);
        // Whitespace-only is the same absence wearing a different byte count.
        assert_eq!(normalize_binding_workdir("   "), ROTATION_UNKNOWN);
        assert_eq!(normalize_binding_workdir("\t\n"), ROTATION_UNKNOWN);
    }

    /// It must NOT invent a workdir, and must not mangle a real one. A binding
    /// registered without a workdir genuinely has none; `unknown` is the honest
    /// rendering, and a guess would be worse than the empty string it replaces.
    #[test]
    fn a_real_workdir_passes_through_byte_for_byte() {
        for wd in [
            "/home/spinak/Projects/qontinui-root",
            "D:/qontinui-root",
            "D:\\qontinui-root\\agent-worktrees\\x",
            // Leading/trailing space around real content is content, not
            // absence — trimming it would change which workdir a row names.
            " /padded/path ",
        ] {
            assert_eq!(normalize_binding_workdir(wd), wd, "must not rewrite {wd}");
        }
    }

    /// The read side holds the invariant for every future construction site,
    /// not just today's three. An unregistered nonce was always `unknown`; the
    /// regression this guards is a LIVE binding leaking an empty workdir into a
    /// `reject` row.
    #[test]
    fn reject_attribution_never_yields_an_empty_workdir() {
        let attr = reject_attribution_for_nonce("");
        assert_eq!(attr.workdir, ROTATION_UNKNOWN);
        assert!(!attr.workdir.is_empty());

        let attr = reject_attribution_for_nonce("a-nonce-that-was-never-registered");
        assert_eq!(attr.workdir, ROTATION_UNKNOWN);
        assert!(!attr.workdir.is_empty());
    }

    /// The two rotation events must not share a throttle bucket. Sharing it
    /// would let a runner-nonce `reject` silence the coord `upstream-reject`
    /// for the same nonce inside one window — the layer conflation this phase
    /// exists to end, reproduced in the log instead of in the response.
    #[test]
    fn upstream_reject_and_nonce_reject_do_not_suppress_each_other() {
        let nonce = format!("{}", uuid::Uuid::new_v4().simple());
        let prefix = rotation_key_prefix(&nonce);

        // Claim the plain `reject` bucket for this prefix.
        assert_eq!(
            reject_throttle_admit(&prefix),
            Some(0),
            "precondition: the reject bucket for this prefix is fresh"
        );
        assert_eq!(
            reject_throttle_admit(&prefix),
            None,
            "precondition: a second reject inside the window is suppressed"
        );

        // The upstream bucket must still be open — a DIFFERENT key.
        assert_eq!(
            reject_throttle_admit(&format!("upstream:{prefix}")),
            Some(0),
            "an upstream-reject must not be suppressed by a nonce-reject in the              same window — they are different events about different layers"
        );
    }

    /// A live binding minted with an empty workdir must surface as the sentinel
    /// rather than as `""` — the 849-row case, exercised end-to-end through the
    /// real mint and the real attribution read.
    #[test]
    fn a_binding_minted_without_a_workdir_attributes_as_unknown() {
        let (nonce, _) =
            mint_and_register_nonce("", ProxyPrincipal::Device, NonceLifetime::Persistent, None);
        let attr = reject_attribution_for_nonce(&nonce);
        assert_eq!(
            attr.workdir, ROTATION_UNKNOWN,
            "an empty workdir must never reach a rotation row as \"\""
        );
        assert_eq!(attr.principal, "device");

        // Leave the process-global map as we found it.
        proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .remove(&nonce);
    }
}

/// Phase 5a of plan
/// `2026-08-31-coord-mcp-credential-selection-by-binding-provenance` — one
/// runner-owned door that answers the question every other door on this box
/// guesses at.
///
/// # Why a new door rather than a fix to `coord_doctor`
///
/// `coord_doctor` answers "is this runner's coord setup correct?" as a
/// pass/fail checklist for an operator. This answers a narrower and more urgent
/// question for a SESSION whose transport just died: *which layer is failing,
/// which credential did the proxy actually select, and is it alive right now?*
/// Phase 3's failure envelope names `GET /coord-mcp/doctor` as the next door,
/// so this is also what keeps that pointer honest — an advertised recovery
/// lever that no code implements is the defect class
/// `planning-and-scope` `finish-to-zero-includes-the-defect-underneath`
/// exists to name, and shipping the envelope without this would have created a
/// fresh one.
///
/// # What it must never do
///
/// Print a token. Every field here is derived — `kid`, `exp`, a usability bit
/// — and [`crate::auth::SlotDescriptor`] structurally cannot carry the secret.
pub(crate) mod doctor {
    use super::*;

    /// The credential the coord-mcp proxy WOULD select for a given tenant,
    /// described without being disclosed.
    #[derive(Debug, Clone, serde::Serialize)]
    pub(crate) struct SelectedSlot {
        /// Which tenant the selection was made for, as the proxy resolves it.
        pub tenant: Option<String>,
        /// How that tenant was decided: `pinned` | `unpinned-default` |
        /// `unresolvable`. This is the field the whole incident turned on — a
        /// session's transport worked or not according to how its tenant
        /// resolved, and nothing reported it.
        pub tenant_source: &'static str,
        /// The slot selection actually returned, described.
        /// `None` means selection MISSED — no bearer would be sent at all,
        /// which is a different failure from sending a dead one.
        pub selected: Option<crate::auth::SlotDescriptor>,
        /// Every per-tenant slot on this box, described. A reader diagnosing
        /// "why did MY session get nothing" needs to see the neighbours: the
        /// operator-box incident was two slots, both long expired, while the
        /// legacy slot was fine.
        pub all_tenant_slots: Vec<TenantSlotView>,
        /// The legacy `access_token` slot, described. `coord_doctor`'s only
        /// direct-door probe authenticates with THIS slot while the proxy
        /// injects the per-tenant one, so showing both side by side is what
        /// makes that divergence visible instead of inferable.
        pub legacy_slot: crate::auth::SlotDescriptor,
    }

    #[derive(Debug, Clone, serde::Serialize)]
    pub(crate) struct TenantSlotView {
        pub tenant: String,
        #[serde(flatten)]
        pub descriptor: crate::auth::SlotDescriptor,
    }

    /// Build the report. Pure over the credential store and the machine pin —
    /// no network, so it answers even when coord is unreachable, which is
    /// precisely when it is asked.
    ///
    /// Deliberately NOT a coord probe: a door that needs coord to answer
    /// "why can't I reach coord" is useless in the case it exists for. The
    /// live-reachability half belongs to `coord_doctor`'s check 8, which
    /// Phase 5a fixes separately.
    pub(crate) fn report() -> serde_json::Value {
        let pin = crate::session::tenant_pin::resolve_tenant_pin();
        let (tenant, tenant_source) = match &pin {
            crate::session::tenant_pin::TenantPin::Pinned(t) => (Some(*t), "pinned"),
            crate::session::tenant_pin::TenantPin::Unpinned => (None, "unpinned-default"),
            crate::session::tenant_pin::TenantPin::Unresolvable => (None, "unresolvable"),
        };

        let am = crate::auth::AuthManager::new();
        let legacy = crate::auth::SlotDescriptor::describe(am.get_access_token().ok().as_deref());

        let all_tenant_slots: Vec<TenantSlotView> = am
            .list_tenant_device_jwt_tenants()
            .into_iter()
            .map(|t| TenantSlotView {
                tenant: t.to_string(),
                descriptor: crate::auth::SlotDescriptor::describe(
                    am.get_tenant_device_jwt(&t).ok().flatten().as_deref(),
                ),
            })
            .collect();

        // The SAME selector the proxy calls. Re-implementing the choice here
        // would let the doctor and the data plane disagree — which is exactly
        // the class of bug this door exists to expose, so it must not be the
        // shape of the door itself.
        let selected = crate::auth::device_bearer_for(tenant.as_ref());
        let selected = selected
            .as_deref()
            .map(|t| crate::auth::SlotDescriptor::describe(Some(t)));

        // `all_tenant_slots` is `.unwrap_or_default()` deep down, so an
        // UNDECRYPTABLE store reads as an empty list. Absence is not zero
        // (`verification-and-evidence` `silent-empty-is-unknown`), and the
        // refresher already learned this lesson the hard way — so an empty
        // list beside a present legacy slot is reported as UNKNOWN rather
        // than as "this box has no tenant slots".
        let slots_are_unknown = all_tenant_slots.is_empty();

        let (verdict, layer, detail) = verdict_for(&selected, &legacy, slots_are_unknown, &pin);

        serde_json::json!({
            "probed_at": chrono::Utc::now().to_rfc3339(),
            "verdict": verdict,
            "layer": layer,
            "detail": detail,
            "credential": SelectedSlot {
                tenant: tenant.map(|t| t.to_string()),
                tenant_source,
                selected,
                all_tenant_slots,
                legacy_slot: legacy,
            },
            "tenant_slots_unknown": slots_are_unknown,
            "slot_health": crate::mcp::device_jwt_refresher::tenant_slot_health()
                .map(|h| serde_json::json!({
                    "observed_at_unix": h.observed_at_unix,
                    "degraded_slots": h.degraded_slots,
                    "cleared_on_expiry_total": h.cleared_on_expiry_total,
                    "cleared_on_rejection_total": h.cleared_on_rejection_total,
                    "slots": h.slots.iter().map(|s| serde_json::json!({
                        "tenant_id": s.tenant_id,
                        "outcome": s.outcome,
                        "clear_cause": s.clear_cause,
                        "rederived": s.rederived,
                        "detail": s.detail,
                    })).collect::<Vec<_>>(),
                })),
            "next_door": next_door_for(layer),
        })
    }

    /// The pure core, so the verdict logic is testable without a credential
    /// store, a machine pin, or a runtime.
    pub(crate) fn verdict_for(
        selected: &Option<crate::auth::SlotDescriptor>,
        legacy: &crate::auth::SlotDescriptor,
        slots_are_unknown: bool,
        pin: &crate::session::tenant_pin::TenantPin,
    ) -> (&'static str, &'static str, String) {
        // A machine whose pin cannot be resolved refuses fail-closed at the
        // proxy, so no credential question is even reached. Reporting a
        // credential verdict here would answer a question that was never asked.
        if matches!(pin, crate::session::tenant_pin::TenantPin::Unresolvable) {
            return (
                "refuses",
                ProxyFailureLayer::RunnerNonce.as_str(),
                "this machine's tenant pin is UNRESOLVABLE, so the proxy refuses \
                 fail-closed before selecting any credential — repair machine.json"
                    .to_string(),
            );
        }
        match selected {
            Some(d) if d.usable => (
                "ok",
                "none",
                format!(
                    "the proxy would send a usable bearer{}{}",
                    d.kid
                        .as_deref()
                        .map(|k| format!(" (kid {k})"))
                        .unwrap_or_default(),
                    d.expires_in_secs
                        .map(|s| format!(", {s}s until exp"))
                        .unwrap_or_default()
                ),
            ),
            // Selection returning a token it would ALSO judge unusable is the
            // Phase-1 defect itself. If this ever fires, selection and the
            // usability predicate have diverged again.
            Some(d) => (
                "degraded",
                ProxyFailureLayer::CoordUpstream.as_str(),
                format!(
                    "selection returned a bearer the same predicate calls UNUSABLE \
                     ({}) — selection and validity have diverged, which is the \
                     Phase-1 defect regressing",
                    d.unusable_reason.unwrap_or("unknown")
                ),
            ),
            None if slots_are_unknown && legacy.usable => (
                "unknown",
                "none",
                "no per-tenant slot could be enumerated (an undecryptable store reads \
                 as EMPTY), while the legacy slot is usable — this is UNKNOWN, not \
                 \"this box has no tenant slots\""
                    .to_string(),
            ),
            None => (
                "no-credential",
                ProxyFailureLayer::CoordUpstream.as_str(),
                format!(
                    "selection MISSED: no usable bearer would be sent at all (legacy slot: {}) \
                     — the refresher re-mints it, or re-pair this runner",
                    legacy.unusable_reason.unwrap_or("usable")
                ),
            ),
        }
    }

    fn next_door_for(layer: &str) -> &'static str {
        if layer == ProxyFailureLayer::RunnerNonce.as_str() {
            ProxyFailureLayer::RunnerNonce.next_door()
        } else if layer == ProxyFailureLayer::CoordUpstream.as_str() {
            ProxyFailureLayer::CoordUpstream.next_door()
        } else {
            "No credential fault is visible from here. If a call is still failing, the \
             fault is at the loopback nonce or in transit — read the failing response's \
             `layer` field, which names which."
        }
    }
}

/// Phase 5a — the credential doctor's verdict logic and its disclosure bound.
#[cfg(test)]
mod coord_mcp_doctor_tests {
    use super::doctor::*;
    use super::*;
    use crate::auth::SlotDescriptor;
    use crate::session::tenant_pin::TenantPin;

    /// A JWT with a `kid` header and a far-future `exp`.
    fn live_jwt() -> String {
        let header = serde_json::json!({"alg": "EdDSA", "kid": "coord-ed25519-abc123"});
        let payload = serde_json::json!({"exp": chrono::Utc::now().timestamp() + 3600});
        encode(&header, &payload)
    }

    /// The same shape, four hours past `exp` — the state the spaceship box's
    /// mint door was measured returning on 2026-09-01.
    fn expired_jwt() -> String {
        let header = serde_json::json!({"alg": "EdDSA", "kid": "coord-ed25519-abc123"});
        let payload = serde_json::json!({"exp": chrono::Utc::now().timestamp() - 14_400});
        encode(&header, &payload)
    }

    fn encode(header: &serde_json::Value, payload: &serde_json::Value) -> String {
        use base64::Engine;
        let b = |v: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!("{}.{}.c2ln", b(header), b(payload))
    }

    /// THE disclosure bound. A diagnostic that leaks the credential it
    /// diagnoses is worse than no diagnostic, and this door is unauthenticated.
    #[test]
    fn a_descriptor_never_carries_the_token() {
        let jwt = live_jwt();
        let d = SlotDescriptor::describe(Some(&jwt));
        let json = serde_json::to_string(&d).expect("serializable");
        assert!(
            !json.contains(&jwt),
            "the token must never appear in a descriptor's serialization"
        );
        // Nor any segment of it — a signature fragment is still key material.
        for seg in jwt.split('.') {
            assert!(
                !json.contains(seg),
                "no JWT segment may appear in the descriptor: {seg}"
            );
        }
        // What it MAY carry: the public identifiers.
        assert_eq!(d.kid.as_deref(), Some("coord-ed25519-abc123"));
        assert!(d.usable);
    }

    /// The three misses are named separately because they have different
    /// repairs — an absent slot needs a mint, an opaque one a re-pair, an
    /// expired one only the refresher. The pre-Phase-1 code collapsed all three
    /// into one falsy bit, which is how a dead slot read as a hit.
    #[test]
    fn the_three_unusable_reasons_are_distinguished() {
        let absent = SlotDescriptor::describe(None);
        assert!(!absent.usable);
        assert_eq!(absent.unusable_reason, Some("absent"));
        assert_eq!(
            SlotDescriptor::describe(Some("   ")).unusable_reason,
            Some("absent")
        );

        // A legacy `qontinui_runner_<random>` bearer: present, unparseable.
        let opaque = SlotDescriptor::describe(Some("qontinui_runner_deadbeef"));
        assert!(!opaque.usable);
        assert_eq!(opaque.unusable_reason, Some("opaque"));
        assert_eq!(opaque.exp, None);

        let expired = SlotDescriptor::describe(Some(&expired_jwt()));
        assert!(!expired.usable);
        assert_eq!(expired.unusable_reason, Some("expired"));
        assert!(
            expired.expires_in_secs.expect("decodable exp") < 0,
            "a past exp must render NEGATIVE seconds — a reader should not have to \
             subtract two epoch integers to see the credential is dead"
        );
    }

    /// `usable` must be the SAME predicate selection uses. If these ever
    /// diverge, the doctor reports a slot as healthy that the proxy would skip
    /// — which is the class of bug this door exists to expose.
    #[test]
    fn usable_agrees_with_the_selection_predicate() {
        for token in [
            live_jwt(),
            expired_jwt(),
            "opaque".to_string(),
            String::new(),
        ] {
            assert_eq!(
                SlotDescriptor::describe(Some(&token)).usable,
                crate::auth::slot_jwt_is_usable(&token),
                "descriptor and selector disagree about {token:?}"
            );
        }
    }

    /// An unresolvable machine pin is answered BEFORE any credential question:
    /// the proxy refuses fail-closed there, so a credential verdict would be
    /// answering something nobody asked.
    #[test]
    fn an_unresolvable_pin_is_reported_as_a_refusal_not_a_credential_fault() {
        let (verdict, layer, detail) = verdict_for(
            &Some(SlotDescriptor::describe(Some(&live_jwt()))),
            &SlotDescriptor::describe(Some(&live_jwt())),
            false,
            &TenantPin::Unresolvable,
        );
        assert_eq!(verdict, "refuses");
        assert_eq!(layer, ProxyFailureLayer::RunnerNonce.as_str());
        assert!(detail.contains("UNRESOLVABLE"));
    }

    /// An empty tenant-slot list is UNKNOWN, not zero — the store is read
    /// through an `.unwrap_or_default()`, so an undecryptable one reads as
    /// empty (`verification-and-evidence` `silent-empty-is-unknown`).
    #[test]
    fn no_enumerable_slots_beside_a_healthy_legacy_slot_is_unknown_not_absent() {
        let (verdict, _, detail) = verdict_for(
            &None,
            &SlotDescriptor::describe(Some(&live_jwt())),
            /* slots_are_unknown */ true,
            &TenantPin::Unpinned,
        );
        assert_eq!(verdict, "unknown");
        assert!(detail.contains("UNKNOWN"));
    }

    /// A selection MISS with slots genuinely enumerated is a real
    /// no-credential verdict, distinct from the unknown above.
    #[test]
    fn a_selection_miss_with_enumerable_slots_is_a_real_no_credential_verdict() {
        let (verdict, layer, _) = verdict_for(
            &None,
            &SlotDescriptor::describe(Some(&expired_jwt())),
            false,
            &TenantPin::Unpinned,
        );
        assert_eq!(verdict, "no-credential");
        assert_eq!(layer, ProxyFailureLayer::CoordUpstream.as_str());
    }

    /// The regression alarm for Phase 1: selection handing back a bearer that
    /// the same predicate calls unusable means validity-selection has broken.
    #[test]
    fn selection_returning_an_unusable_bearer_is_reported_as_the_phase_1_regression() {
        let (verdict, _, detail) = verdict_for(
            &Some(SlotDescriptor::describe(Some(&expired_jwt()))),
            &SlotDescriptor::describe(None),
            false,
            &TenantPin::Unpinned,
        );
        assert_eq!(verdict, "degraded");
        assert!(detail.contains("diverged"));
    }

    /// End-to-end over the REAL credential store on whatever box runs this:
    /// the report must be total (never panic, whatever the store holds), must
    /// carry every field the route's consumers read, and must not contain a
    /// credential.
    ///
    /// This is what makes the Phase-3 envelope's `next_door` pointer honest —
    /// it names `GET /coord-mcp/doctor`, and this asserts the thing behind that
    /// route actually answers.
    #[test]
    fn the_live_report_is_total_and_discloses_no_credential() {
        let r = report();
        for field in [
            "probed_at",
            "verdict",
            "layer",
            "detail",
            "credential",
            "tenant_slots_unknown",
            "next_door",
        ] {
            assert!(r.get(field).is_some(), "report is missing `{field}`");
        }
        assert!(
            chrono::DateTime::parse_from_rfc3339(r["probed_at"].as_str().expect("string")).is_ok(),
            "probed_at must be RFC3339"
        );

        // Whatever this box's store holds, no value in the payload may be
        // JWT-SHAPED. A `kid` and an `exp` are fine; three dot-separated
        // base64url segments are not.
        fn walk(v: &serde_json::Value, path: &str) {
            match v {
                serde_json::Value::String(s) => {
                    let segs: Vec<&str> = s.split('.').collect();
                    let jwt_shaped = segs.len() == 3
                        && segs.iter().all(|p| {
                            !p.is_empty()
                                && p.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        });
                    assert!(
                        !jwt_shaped,
                        "a JWT-shaped string reached the doctor payload at {path}"
                    );
                }
                serde_json::Value::Array(a) => {
                    for (i, x) in a.iter().enumerate() {
                        walk(x, &format!("{path}[{i}]"));
                    }
                }
                serde_json::Value::Object(o) => {
                    for (k, x) in o {
                        walk(x, &format!("{path}.{k}"));
                    }
                }
                _ => {}
            }
        }
        walk(&r, "$");
    }

    /// The happy path names the kid and the remaining lifetime, because "it
    /// works" without those is not a diagnosis anyone can act on next time.
    #[test]
    fn a_healthy_selection_reports_ok_with_the_kid_and_lifetime() {
        let (verdict, layer, detail) = verdict_for(
            &Some(SlotDescriptor::describe(Some(&live_jwt()))),
            &SlotDescriptor::describe(Some(&live_jwt())),
            false,
            &TenantPin::Pinned(uuid::Uuid::new_v4()),
        );
        assert_eq!(verdict, "ok");
        assert_eq!(layer, "none");
        assert!(detail.contains("coord-ed25519-abc123"));
        assert!(detail.contains("until exp"));
    }
}
