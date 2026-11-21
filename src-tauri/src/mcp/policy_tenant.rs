//! Which tenant's policy a session is briefed under — the tenant decision
//! behind the SessionStart policy injection ([`super::policy_context`]) and the
//! spawn-time policy body ([`crate::session::spawn_prompt`]) (plan
//! `2026-11-13-runner-policy-injection-uses-the-device-default-tenant-not-the-sessions`,
//! Phases 1-2).
//!
//! ## The defect this closes
//!
//! On a device bound to more than one tenant, both deliveries used to read with
//! the device's ONE default credential: the SessionStart route fetched through
//! `continuation_verdict::coord_client_parts()` and cached per coord base, and
//! the spawn seams read the cache file named for the default JWT's tenant. A
//! session working another tenant's repo was therefore told "this is the
//! canonical text of `policy/session-protocol`" for a tenant whose documents do
//! not govern it — measured on device c79a07d5, where a bauhaus-b2b worktree
//! session was handed qontinui's v13 protocol while its own coord-mcp served
//! bauhaus-b2b's v2.
//!
//! ## One resolver, not two
//!
//! The session's tenant is the one the coord-mcp proxy already decides for
//! every call the session makes ([`crate::coord_mcp::decide_session_tenant`]),
//! read through [`crate::coord_mcp::terminal_session_bearer_and_tenant`] — the
//! AWAITING door, because SessionStart is the session's first call and the
//! read-only census paths would answer a repo-steered session from the machine
//! rows. When the route's id maps to no nonce, the session registry's stamped
//! tenant is the fallback ([`crate::session::session_tenant_scope`]) — the same
//! credential → data-plane precedence `commands/session_tenant_display.rs`
//! applies. This module adds no third source; it only DECIDES from those reads
//! ([`policy_tenant_decision`]), and that decision is pure.
//!
//! ## UNKNOWN is never the default
//!
//! On a multi-bound device a session whose tenant cannot be resolved — no nonce
//! and no registry answer, a proxy refusal, a repo still unsettled after the
//! proxy's budget, or a resolved tenant this runner holds no usable credential
//! for — gets the fail-open "fetch it yourself" notice, counted under
//! [`super::policy_context::PolicyRenderReason::TenantUnresolved`], never the
//! default tenant's documents (served `verification-and-evidence`
//! `unknown-must-not-render-as-a-default`). On a single-bound device the sole
//! tenant is proven, so an unresolvable read falls back to the default slot and
//! behaves as before.
//!
//! ## The Stop hook and session close decide the same way
//!
//! The Stop-hook readers — the work-status read and the continuation rules
//! ([`super::continuation_verdict`]), the compliance config read and the
//! compliance POST ([`super::session_compliance`]) — take ONE decision per Stop
//! from [`route_policy_target`], bounded by [`STOP_TENANT_BUDGET`], and the
//! close-time compliance finalize takes one from the lifecycle record
//! ([`closed_session_policy_target`]) (plan
//! `2026-11-13-runner-stop-hook-continuation-rules-and-compliance-use-the-device-default-tenant`).
//! Every request any of them builds passes [`build_checked`] before it is sent,
//! so none leaves without a credential claiming the tenant it was decided for.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use tracing::debug;
use uuid::Uuid;

use super::policy_context::Mode;
use crate::auth::TenantScope;

// ===========================================================================
// The read and the decision (pure)
// ===========================================================================

/// What the runner READ about one session's tenant, before anything is decided.
///
/// Carries no bearer token — only whether a usable one exists — so a `{:?}` of
/// it can never put a credential in a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyTenantRead {
    /// The coord-mcp proxy selected `tenant`'s slot for the session's own
    /// nonce; `credential` says whether that slot holds a usable bearer.
    ProxyOwned { tenant: Uuid, credential: bool },
    /// The coord-mcp proxy selected the DEFAULT slot for the session's nonce
    /// (`Ok(None)`): the session's coord-mcp calls present the default
    /// binding's credential, so the policy that matches them is the default
    /// credential's tenant's.
    ProxyDefaultSlot,
    /// The coord-mcp proxy refused to name a tenant for the session's nonce.
    /// `code` is the proxy's stable refusal code
    /// ([`crate::coord_mcp::TENANT_REPO_UNSETTLED_CODE`] for the first-call
    /// race that outlived its budget).
    ProxyRefused { code: &'static str, message: String },
    /// No nonce names the session; the session registry holds `tenant` for it.
    Registry { tenant: Uuid, credential: bool },
    /// Neither leg named a tenant; the reason says why.
    Unnamed(String),
}

/// Which tenant's policy one session gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyTenantDecision {
    /// Fetch under `tenant`'s own credential and file the body under it.
    Owned(Uuid),
    /// Fetch under the default slot (`TenantScope::Device`). Reached when the
    /// proxy itself answered the default slot, or on a single-bound device
    /// whose tenant read was inconclusive — the sole tenant is proven there.
    DefaultSlot,
    /// UNKNOWN: inject the fail-open notice naming this cause, fetch nothing.
    Unknown(String),
}

/// Decide whose policy a session gets from what was read about it.
///
/// | Read | Single-bound | Multi-bound |
/// |---|---|---|
/// | proxy/registry names `t`, usable credential | `Owned(t)` | `Owned(t)` |
/// | proxy/registry names `t`, NO usable credential | `Unknown` | `Unknown` |
/// | proxy answered the default slot | `DefaultSlot` | `DefaultSlot` |
/// | proxy refused (incl. repo unsettled) | `DefaultSlot` | `Unknown` |
/// | nothing named a tenant | `DefaultSlot` | `Unknown` |
///
/// The no-credential row is `Unknown` on EVERY device (Design §2a): the
/// tenant-stating fetch sends UNAUTHENTICATED when `t` has no usable slot, and
/// whatever answered such a fetch would then be filed under `t`'s name.
pub(crate) fn policy_tenant_decision(
    read: PolicyTenantRead,
    multi_bound: bool,
) -> PolicyTenantDecision {
    match read {
        PolicyTenantRead::ProxyOwned {
            tenant,
            credential: true,
        }
        | PolicyTenantRead::Registry {
            tenant,
            credential: true,
        } => PolicyTenantDecision::Owned(tenant),
        PolicyTenantRead::ProxyOwned {
            tenant,
            credential: false,
        }
        | PolicyTenantRead::Registry {
            tenant,
            credential: false,
        } => PolicyTenantDecision::Unknown(format!(
            "this session acts as tenant {tenant}, and this runner holds no usable credential \
             for tenant {tenant}, so it did not fetch: a fetch presenting no credential (or \
             another tenant's) could be answered with another tenant's policy"
        )),
        PolicyTenantRead::ProxyDefaultSlot => PolicyTenantDecision::DefaultSlot,
        PolicyTenantRead::ProxyRefused { code, message } if multi_bound => {
            PolicyTenantDecision::Unknown(unknown_on_multi_bound(&format!(
                "the coord-mcp proxy refused to name one ({code}: {message})"
            )))
        }
        PolicyTenantRead::Unnamed(reason) if multi_bound => {
            PolicyTenantDecision::Unknown(unknown_on_multi_bound(&reason))
        }
        PolicyTenantRead::ProxyRefused { .. } | PolicyTenantRead::Unnamed(_) => {
            PolicyTenantDecision::DefaultSlot
        }
    }
}

/// The UNKNOWN cause for a multi-bound device, phrased for the notice.
fn unknown_on_multi_bound(cause: &str) -> String {
    format!(
        "this session's tenant is UNKNOWN on a device bound to several tenants — {cause}. The \
         runner did not inject the device default tenant's policy, which may not be the policy \
         that governs this session's repo"
    )
}

/// Where one SessionStart fetch goes: the credential scope it presents and the
/// tenant that credential must claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PolicyTarget {
    /// Passed to `coord_http::coord_get_for`.
    pub(crate) scope: TenantScope,
    /// The tenant whose policy this is: keys the in-process cache, names the
    /// spawn cache file, and renders the `[tenant: …]` line. Every fetch must
    /// present a credential claiming exactly this tenant. `None` only for a
    /// default slot whose credential names no tenant.
    pub(crate) tenant: Option<Uuid>,
}

/// The fetch target for a decision — or, for [`PolicyTenantDecision::Unknown`],
/// the reason there is none. No target means no fetch: that is how §2a's
/// "do not fetch" holds by construction. `default_slot_tenant` is read only on
/// the default-slot arm.
pub(crate) fn fetch_target(
    decision: PolicyTenantDecision,
    default_slot_tenant: impl FnOnce() -> Option<Uuid>,
) -> Result<PolicyTarget, String> {
    match decision {
        PolicyTenantDecision::Owned(t) => Ok(PolicyTarget {
            scope: TenantScope::Owned(t),
            tenant: Some(t),
        }),
        PolicyTenantDecision::DefaultSlot => Ok(PolicyTarget {
            scope: TenantScope::Device,
            tenant: default_slot_tenant(),
        }),
        PolicyTenantDecision::Unknown(reason) => Err(reason),
    }
}

/// The tenant whose cached body a spawn may compose for this decision, or
/// `None` to compose none. `default_slot_tenant` is read only on the
/// default-slot arm.
pub(crate) fn spawn_body_tenant(
    decision: &PolicyTenantDecision,
    default_slot_tenant: impl FnOnce() -> Option<Uuid>,
) -> Option<Uuid> {
    match decision {
        PolicyTenantDecision::Owned(t) => Some(*t),
        PolicyTenantDecision::DefaultSlot => default_slot_tenant(),
        PolicyTenantDecision::Unknown(_) => None,
    }
}

/// The tenant a fetched body's spawn cache file may be named for: the claim of
/// the bearer the fetch actually PRESENTED, and only when it equals the
/// session's tenant (Design §2a). Anything else names no file, so no body can
/// be filed under a tenant whose credential did not fetch it.
pub(crate) fn cache_file_tenant(
    session_tenant: Option<Uuid>,
    presented: Option<Uuid>,
) -> Option<Uuid> {
    match (session_tenant, presented) {
        (Some(session), Some(presented)) if session == presented => Some(presented),
        _ => None,
    }
}

/// How a reader names the tenant it acted under, for its log line and the
/// continuation verdict's `tenant` / `tenant_reason` fields: `(tenant, reason)`.
///
/// `tenant` is short and greppable — the target tenant's uuid, `default-slot`
/// when the default credential claims no tenant, or `unknown` when the decision
/// gave no target — and `reason` carries the why for the last two, so a log
/// query on the tenant never has to match a sentence.
pub(crate) fn target_label(target: &Result<PolicyTarget, String>) -> (String, Option<String>) {
    match target {
        Ok(PolicyTarget {
            tenant: Some(t), ..
        }) => (t.to_string(), None),
        Ok(PolicyTarget { tenant: None, .. }) => (
            "default-slot".to_string(),
            Some("the default credential claims no tenant".to_string()),
        ),
        Err(cause) => ("unknown".to_string(), Some(cause.clone())),
    }
}

// ===========================================================================
// The presented credential (the last-moment guard, pure)
// ===========================================================================

/// The tenant a device JWT names, from its unverified `tenant_id` claim.
///
/// Used only to NAME a local cache file and to check that a request presents
/// the target tenant's credential, never to authorize anything — the same
/// posture `coord_mcp::device_jwt_claim_tenant` takes. Coord resolves the rows
/// a request reads or writes from this same bearer, so the claim is the right
/// scope for both.
fn tenant_of_jwt(jwt: &str) -> Option<Uuid> {
    let raw = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(jwt.trim())?;
    Uuid::parse_str(&raw).ok()
}

/// The bearer token a built request will present, off its own
/// `Authorization` header. `None` for no header, a non-bearer one, or an empty
/// token. The token goes no further than [`tenant_of_jwt`] and a presence test.
fn presented_bearer(headers: &reqwest::header::HeaderMap) -> Option<&str> {
    let value = headers.get(reqwest::header::AUTHORIZATION)?.to_str().ok()?;
    value
        .trim()
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The tenant a built request's bearer claims — the credential it will
/// ACTUALLY present, read off its `Authorization` header rather than
/// re-resolved, so the check below and the cache-file name are about the bytes
/// on the wire. `None` for no header, a non-bearer one, or a token whose
/// `tenant_id` claim is absent or not a uuid. The token itself goes no further.
pub(crate) fn presented_claim(headers: &reqwest::header::HeaderMap) -> Option<Uuid> {
    tenant_of_jwt(presented_bearer(headers)?)
}

/// Refuse to send a coord request whose credential is not the target tenant's.
///
/// `coord_get_for(Owned(t))` / `coord_post_for(Owned(t))` send UNAUTHENTICATED
/// when `t` has no usable slot (and a slot can lapse between the tenant
/// decision and the request), and an answer to such a request could be another
/// tenant's — which would then be rendered, cached and filed as `t`'s, or a
/// write would land wherever coord's anonymous resolution put it. The tenant
/// decision already refuses the no-credential case up front
/// ([`policy_tenant_decision`]); this is the same rule at the last moment, on
/// the request that would go out. A target naming no tenant (a default slot
/// whose credential claims none) asserts nothing, so it checks nothing.
pub(crate) fn check_presented(target: Option<Uuid>, presented: Option<Uuid>) -> Result<(), String> {
    match (target, presented) {
        (Some(t), Some(p)) if t == p => Ok(()),
        (Some(t), Some(p)) => Err(format!(
            "the credential this runner would present for tenant {t} claims tenant {p}, so the \
             coord request was not sent"
        )),
        (Some(t), None) => Err(format!(
            "this runner holds no usable credential claiming tenant {t}, so the coord request \
             was not sent"
        )),
        (None, _) => Ok(()),
    }
}

/// How long building one tenant-stating request may take. Building it resolves
/// the credential (`coord_get_for` / `coord_post_for` read the slot store, and
/// may reach the OS keychain), which is synchronous — so it runs off the async
/// runtime under this bound ([`build_off_runtime`]). Normally milliseconds.
pub(crate) const CREDENTIAL_BUILD_TIMEOUT: Duration = Duration::from_secs(1);

/// Run `build` — a closure returning a [`build_checked`] request — on a
/// blocking thread under [`CREDENTIAL_BUILD_TIMEOUT`], so a slow secure-storage
/// or keychain read never stalls an async worker or outlives a caller's budget.
pub(crate) async fn build_off_runtime<T: Send + 'static>(
    build: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    match tokio::time::timeout(CREDENTIAL_BUILD_TIMEOUT, tokio::task::spawn_blocking(build)).await {
        Ok(Ok(built)) => built,
        Ok(Err(e)) => Err(format!("credential resolution failed ({e})")),
        Err(_) => Err(format!(
            "credential resolution did not finish within {} ms, so the coord request was not sent",
            CREDENTIAL_BUILD_TIMEOUT.as_millis()
        )),
    }
}

/// Build `rb` — a request from `coord_http::coord_get_for` /
/// `coord_post_for` under `target.scope` — and refuse it BEFORE it can be sent
/// unless its bearer claims `target.tenant` ([`check_presented`]) and, for a
/// target naming no tenant, unless it presents a bearer at all.
///
/// The second half is what keeps "unpaired ⇒ inert" true on the tenant-stating
/// seam: those helpers send UNAUTHENTICATED when no slot resolves, and a
/// default slot whose credential claims no tenant gives `check_presented`
/// nothing to compare. Returns the request and the tenant its bearer claims.
pub(crate) fn build_checked(
    rb: reqwest::RequestBuilder,
    target: PolicyTarget,
) -> Result<(reqwest::Request, Option<Uuid>), String> {
    let request = rb
        .build()
        .map_err(|e| format!("could not build the coord request: {e}"))?;
    let presented = presented_claim(request.headers());
    check_presented(target.tenant, presented)?;
    if presented_bearer(request.headers()).is_none() {
        return Err(
            "this runner holds no usable device credential (unpaired, or its default slot is \
             not usable), so the coord request was not sent"
                .to_string(),
        );
    }
    Ok((request, presented))
}

// ===========================================================================
// The spawn-time body (local I/O only)
// ===========================================================================

/// The policy body a spawn seam should compose into the system prompt for
/// `tenant`, or `None` for "stay inline".
///
/// Local I/O only, never a coord call: the injection flag must be `on`
/// (`off`/`observe` inject nothing, and a system prompt is an injection) and
/// `tenant`'s cache must exist. The tenant is an INPUT — each spawn seam
/// resolves the tenant it holds ([`new_session_policy_tenant`],
/// [`resolve_shell_pane_policy_tenant`]) — so no seam can compose the device
/// default's body for a session of another tenant.
pub fn spawn_policy_body(tenant: Option<Uuid>) -> Option<String> {
    spawn_policy_body_in(
        &crate::session::claude_hook::session_restore_dir(),
        Mode::from_env(),
        tenant,
    )
}

/// [`spawn_policy_body`] over an explicit cache dir and mode — the unit-test
/// surface. `None` tenant composes nothing; there is no unscoped fallback name.
pub(crate) fn spawn_policy_body_in(
    base_dir: &Path,
    mode: Mode,
    tenant: Option<Uuid>,
) -> Option<String> {
    if mode != Mode::On {
        return None;
    }
    crate::session::spawn_prompt::read_policy_body_cache(base_dir, &tenant?)
}

// ===========================================================================
// The reads (impure)
// ===========================================================================

/// The tenant the DEFAULT slot's credential claims — what a default-slot fetch
/// presents, read through the same resolver the request path uses
/// ([`crate::auth::presented_tenant`]). The token itself never leaves that call.
pub(crate) fn default_slot_tenant() -> Option<Uuid> {
    match crate::auth::presented_tenant(TenantScope::Device) {
        crate::auth::PresentedTenant::Tenant(t) => Some(t),
        crate::auth::PresentedTenant::Untenanted | crate::auth::PresentedTenant::Anonymous(_) => {
            None
        }
    }
}

/// Is this device multi-bound? The proxy's own row-1d input
/// ([`crate::coord_mcp::device_is_multi_bound`]), read off the runtime: an
/// unreadable store, or a read that did not complete, is `true` — the
/// conservative arm, since its failure mode is a cross-tenant briefing.
pub(crate) async fn device_multi_bound() -> bool {
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(
        crate::coord_mcp::device_is_multi_bound,
    )
    .await
    .unwrap_or(true)
}

/// Read one session's tenant: the coord-mcp proxy's decision for the nonce of
/// the session in `terminal_id` (whose cwd is `workdir`), else the session
/// registry's stamp for `registry_session`.
pub(crate) async fn read_session_policy_tenant(
    terminal_id: &str,
    workdir: Option<&str>,
    registry_session: Option<Uuid>,
) -> PolicyTenantRead {
    use crate::coord_mcp::TerminalSessionTenant;
    match crate::coord_mcp::terminal_session_bearer_and_tenant(terminal_id, workdir).await {
        TerminalSessionTenant::Answered(Ok((Some(tenant), bearer))) => {
            PolicyTenantRead::ProxyOwned {
                tenant,
                credential: bearer.is_some(),
            }
        }
        TerminalSessionTenant::Answered(Ok((None, _))) => PolicyTenantRead::ProxyDefaultSlot,
        TerminalSessionTenant::Answered(Err(refusal)) => PolicyTenantRead::ProxyRefused {
            code: refusal.code,
            message: refusal.message,
        },
        TerminalSessionTenant::NoNonce(why) => registry_leg(why, registry_session).await,
    }
}

/// The registry fallback for a session no nonce names.
async fn registry_leg(why: &'static str, registry_session: Option<Uuid>) -> PolicyTenantRead {
    // `session_tenant_scope` takes the registry lock and the credential check
    // reads the slot store — both off the runtime.
    let joined = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        match crate::session::session_tenant_scope(registry_session) {
            TenantScope::Owned(t) => Some((t, crate::auth::device_holds_usable_binding(&t))),
            TenantScope::Device | TenantScope::Unresolved => None,
        }
    })
    .await;
    match joined {
        Ok(Some((tenant, credential))) => PolicyTenantRead::Registry { tenant, credential },
        Ok(None) => PolicyTenantRead::Unnamed(format!(
            "no coord-mcp session key names this session ({why}), and the session registry \
             holds no tenant for it"
        )),
        Err(e) => PolicyTenantRead::Unnamed(format!(
            "no coord-mcp session key names this session ({why}), and the session-registry read \
             did not complete ({e})"
        )),
    }
}

/// A hook route's fetch target for the session behind `/sessions/{route_id}/…`
/// — the SessionStart policy injection (under [`ROUTE_TENANT_BUDGET`]) and the
/// Stop-hook continuation verdict (under [`STOP_TENANT_BUDGET`]). `Err` is the
/// UNKNOWN cause: no request is made for it.
///
/// `route_id` is the runner TERMINAL id the hook sends (falling back to the
/// Claude session id); `session_key` is what `mcp::sessions::resolve_session_key`
/// made of it (an `AiCoordRegistrar`-mapped coord session id, else the path id).
/// The proxy leg reads the terminal's cwd, else `fallback_workdir` (the Stop
/// payload's `cwd`). The registry fallback asks for the terminal's own coord
/// row first — the leg the session-info tenancy block calls the data plane —
/// then for `session_key`.
pub(crate) async fn route_policy_target(
    route_id: &str,
    session_key: &str,
    fallback_workdir: Option<&str>,
    budget: Duration,
) -> Result<PolicyTarget, String> {
    let (workdir, registry_session) = route_session_ref(route_id, session_key, fallback_workdir);
    decide_target_within(
        budget,
        read_session_policy_tenant(route_id, workdir.as_deref(), registry_session),
        device_multi_bound(),
        default_slot_tenant_off_runtime(),
    )
    .await
}

/// How much of a decision's budget is held back from the tenant READ for the
/// rest of the decision — the binding count and, on the default-slot arm, the
/// default credential's claim. Both are local file reads off the runtime, so
/// an unsettled read still leaves them room to decide `DefaultSlot` on a
/// single-bound device rather than timing the whole decision out.
pub(crate) const DECISION_RESERVE: Duration = Duration::from_millis(500);

/// The whole tenant decision under ONE deadline (`budget`): the read (bounded
/// at `budget - DECISION_RESERVE`, past which it is `Unnamed`), the binding
/// count — skipped when the read decides on its own ([`needs_binding_count`])
/// — and the default credential's claim on the default-slot arm.
///
/// A decision that has not finished by `budget` is UNKNOWN: by then the
/// binding count has not settled either, and the count is what would have
/// licensed the default slot (an unreadable count is already read as
/// multi-bound, [`device_multi_bound`]). Generic over its three reads so the
/// timing is testable on a paused clock.
pub(crate) async fn decide_target_within(
    budget: Duration,
    read: impl Future<Output = PolicyTenantRead>,
    multi_bound: impl Future<Output = bool>,
    default_slot_tenant: impl Future<Output = Option<Uuid>>,
) -> Result<PolicyTarget, String> {
    let read_budget = budget.saturating_sub(DECISION_RESERVE);
    let decided = tokio::time::timeout(budget, async {
        let read = read_within(read_budget, read).await;
        let multi_bound = if needs_binding_count(&read) {
            multi_bound.await
        } else {
            false
        };
        match policy_tenant_decision(read, multi_bound) {
            PolicyTenantDecision::DefaultSlot => {
                let claimed = default_slot_tenant.await;
                fetch_target(PolicyTenantDecision::DefaultSlot, || claimed)
            }
            decision => fetch_target(decision, || None),
        }
    })
    .await;
    decided.unwrap_or_else(|_| {
        Err(format!(
            "this session's tenant is UNKNOWN — its tenant decision did not complete within \
             its {} ms budget, so the device's bindings were not counted in time. The runner \
             did not fall back to the device default tenant, which may not be the tenant that \
             governs this session's repo",
            budget.as_millis()
        ))
    })
}

/// Does deciding on `read` need the device's binding count? Only the two
/// inconclusive reads do: a named tenant (with or without a usable credential)
/// and the proxy's default-slot answer decide the same on every device
/// ([`policy_tenant_decision`]), so for them the count is a wasted
/// secure-storage read on every Stop.
pub(crate) fn needs_binding_count(read: &PolicyTenantRead) -> bool {
    matches!(
        read,
        PolicyTenantRead::ProxyRefused { .. } | PolicyTenantRead::Unnamed(_)
    )
}

/// [`default_slot_tenant`] off the runtime: it reads the encrypted slot store,
/// and may reach the OS keychain. A join failure is no claim.
async fn default_slot_tenant_off_runtime() -> Option<Uuid> {
    qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(default_slot_tenant)
        .await
        .ok()
        .flatten()
}

/// `read`, or — when it has not answered within `budget` — an `Unnamed` read
/// saying so. Unnamed decides UNKNOWN on a multi-bound device and the default
/// slot on a single-bound one ([`policy_tenant_decision`]), so a slow tenant
/// read can never stall the caller past its budget.
pub(crate) async fn read_within(
    budget: Duration,
    read: impl Future<Output = PolicyTenantRead>,
) -> PolicyTenantRead {
    match tokio::time::timeout(budget, read).await {
        Ok(read) => read,
        Err(_) => PolicyTenantRead::Unnamed(format!(
            "the session's tenant did not settle within its {} ms budget",
            budget.as_millis()
        )),
    }
}

/// How long the SessionStart route waits for the session's tenant. The hook
/// calls the route under `curl --max-time 15` and swallows a timeout, so a read
/// that used the proxy's whole `CWD_TENANT_BUDGET` (10 s) before a 4 s fetch
/// would leave under a second of margin and, past it, deliver neither the body
/// nor the fail-open notice. Capping the decision keeps the notice
/// deliverable: an unsettled read is `Unnamed`, which the decision turns into
/// UNKNOWN on a multi-bound device and the default slot on a single-bound one.
pub(crate) const ROUTE_TENANT_BUDGET: Duration = Duration::from_secs(8);

/// How long the Stop route's whole tenant decision may take (plan
/// `2026-11-13-runner-stop-hook-continuation-rules-and-compliance-use-the-device-default-tenant`
/// Design §4). It runs before the compliance turn and the continuation reads,
/// and all of them must fit the hook's `curl --max-time 10` — the sum is
/// pinned by `continuation_verdict`'s
/// `the_stop_paths_worst_case_fits_the_hooks_curl_max_time`. SessionStart has
/// already settled the proxy's expectation for a live session, so the await
/// normally returns at once, and the verdict caches an answered decision.
pub(crate) const STOP_TENANT_BUDGET: Duration = Duration::from_secs(2);

/// `(workdir, registry session id)` for the route's session: the hosting
/// terminal's cwd (else `fallback_workdir`) and coord row when `route_id` names
/// a live terminal, else `session_key` read as a registry id.
fn route_session_ref(
    route_id: &str,
    session_key: &str,
    fallback_workdir: Option<&str>,
) -> (Option<String>, Option<Uuid>) {
    let (workdir, row) = live_terminal_ref(route_id);
    (
        workdir.or_else(|| non_empty(fallback_workdir)),
        row.or_else(|| Uuid::parse_str(session_key.trim()).ok()),
    )
}

/// `(cwd, coord row)` of the live terminal `terminal_id` names, each `None`
/// when there is no such terminal or it holds none.
fn live_terminal_ref(terminal_id: &str) -> (Option<String>, Option<Uuid>) {
    use tauri::Manager;
    let terminal = crate::tauri_app_handle::current().and_then(|app| {
        app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
            .and_then(|tm| tm.get(terminal_id))
    });
    let workdir = terminal
        .as_ref()
        .and_then(|t| non_empty(Some(t.working_dir())));
    let row = terminal.and_then(|t| t.coord_session_id());
    (workdir, row)
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The fetch target for a session whose lifecycle record just CLOSED — the
/// close-time compliance finalize (Design §5). It reads through the same
/// [`read_session_policy_tenant`] as the hook routes, from the record: its
/// `terminal_id`, its `working_dir`, and its coord row. By close the terminal's
/// nonce may be released (`NoNonce`), and then the registry leg decides.
///
/// After a runner restart the in-process registry knows nothing about a
/// session the boot sweep closes, so the registry leg reads `Unnamed` for
/// every one of them. The record's DURABLE `tenant_id` (stamped at spawn) is
/// then the answer ([`recorded_fallback_tenant`]), read as a registry answer:
/// `Owned` when this runner holds a usable credential for it, UNKNOWN when not.
pub(crate) async fn closed_session_policy_target(
    terminal_id: &str,
    harness_session_id: &str,
    recorded_workdir: Option<&str>,
    recorded_row: Option<Uuid>,
    recorded_tenant: Option<Uuid>,
    budget: Duration,
) -> Result<PolicyTarget, String> {
    let (live_workdir, live_row) = live_terminal_ref(terminal_id);
    let (workdir, registry_session) = close_session_ref(
        recorded_workdir,
        live_workdir.as_deref(),
        registrar_row(harness_session_id),
        recorded_row,
        live_row,
    );
    let read = live_read_then_recorded(
        budget.saturating_sub(DECISION_RESERVE * 2),
        read_session_policy_tenant(terminal_id, workdir.as_deref(), registry_session),
        recorded_tenant,
        |tenant| async move {
            qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                crate::auth::device_holds_usable_binding(&tenant)
            })
            .await
            .unwrap_or(false)
        },
    );
    decide_target_within(
        budget,
        read,
        device_multi_bound(),
        default_slot_tenant_off_runtime(),
    )
    .await
}

/// A closed session's read: the LIVE leg bounded by `live_budget` on its own,
/// then the record's durable tenant when the live leg named none (or did not
/// answer in time — a timed-out live leg is `Unnamed`). Bounding only the live
/// leg is the point: a slow proxy or registry read must not hide the record's
/// answer by eating the whole decision budget. `credential` says whether this
/// runner holds a usable credential for the recorded tenant.
pub(crate) async fn live_read_then_recorded<C, F>(
    live_budget: Duration,
    live: impl Future<Output = PolicyTenantRead>,
    recorded: Option<Uuid>,
    credential: C,
) -> PolicyTenantRead
where
    C: FnOnce(Uuid) -> F,
    F: Future<Output = bool>,
{
    let live = read_within(live_budget, live).await;
    match recorded_fallback_tenant(&live, recorded) {
        Some(tenant) => PolicyTenantRead::Registry {
            tenant,
            credential: credential(tenant).await,
        },
        None => live,
    }
}

/// The lifecycle record's durable tenant, when the live read named none
/// (`Unnamed`) — and only then: a proxy answer, a proxy refusal, or a registry
/// answer is the session's CURRENT state and outranks a spawn-time stamp. Pure.
pub(crate) fn recorded_fallback_tenant(
    live: &PolicyTenantRead,
    recorded: Option<Uuid>,
) -> Option<Uuid> {
    match live {
        PolicyTenantRead::Unnamed(_) => recorded,
        _ => None,
    }
}

/// The coord row this runner registered for `harness_session_id` since boot
/// (the `AiCoordRegistrar`'s reverse map), if any.
fn registrar_row(harness_session_id: &str) -> Option<Uuid> {
    use tauri::Manager;
    crate::tauri_app_handle::current().and_then(|app| {
        app.try_state::<std::sync::Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
            .and_then(|r| r.session_id_for(harness_session_id))
    })
}

/// `(workdir, registry session id)` for a closed record. Pure.
///
/// The record's own `working_dir` first, the terminal's live cwd second. The
/// coord row is the one keyed by the session's OWN harness id first (the
/// registrar's map), then the row the record itself names
/// (`finish_coord_session_id`), and the terminal's live row last: a pane hosts
/// sessions in sequence, so by close the terminal can already be bound to the
/// NEXT session's row.
fn close_session_ref(
    recorded_workdir: Option<&str>,
    live_workdir: Option<&str>,
    registrar_row: Option<Uuid>,
    recorded_row: Option<Uuid>,
    live_row: Option<Uuid>,
) -> (Option<String>, Option<Uuid>) {
    (
        non_empty(recorded_workdir).or_else(|| non_empty(live_workdir)),
        registrar_row.or(recorded_row).or(live_row),
    )
}

/// The tenant a DIRECT-EXEC spawn in `workdir` composes the policy body for
/// (Design §6): `NewSessionTenant::Stated(t)` from
/// [`crate::session::resolve_new_session_tenant_in`] — the same authority order
/// the proxy applies to a session nobody named a tenant for — and nothing on
/// any other answer, which leaves the spawn on the inline briefing while the
/// SessionStart route delivers the full body or the UNKNOWN notice.
///
/// On a MULTI-BOUND device an answer the repo tier did not settle is not good
/// enough: `resolve_new_session_tenant_in` then falls through to the machine
/// rows (the device default), and composing that tenant's body would brief the
/// session under another tenant's documents in exactly the first-call race the
/// SessionStart route refuses as UNKNOWN. [`new_session_body_tenant`] decides.
///
/// Skips the resolution (bounded by `repo_tenant::CWD_TENANT_BUDGET`) when the
/// injection is not `on`, because no body would be composed anyway.
pub(crate) async fn new_session_policy_tenant(workdir: &Path) -> Option<Uuid> {
    if Mode::from_env() != Mode::On {
        return None;
    }
    let (decided, repo_settled) =
        crate::session::resolve_new_session_tenant_settled_in(Some(workdir)).await;
    let multi_bound = if repo_settled {
        false
    } else {
        device_multi_bound().await
    };
    new_session_body_tenant(decided.tenant(), repo_settled, multi_bound)
}

/// The pure half of [`new_session_policy_tenant`]: the stated tenant, unless
/// the repo answer did not settle on a multi-bound device — then nothing, and
/// the SessionStart route delivers the body or the UNKNOWN notice. A
/// single-bound device keeps the machine-row answer, which is its sole tenant.
pub(crate) fn new_session_body_tenant(
    stated: Option<Uuid>,
    repo_settled: bool,
    multi_bound: bool,
) -> Option<Uuid> {
    if multi_bound && !repo_settled {
        return None;
    }
    stated
}

/// How long a SHELL pane's spawn waits for its session tenant before composing
/// no body. The pane spawn is synchronous and an operator is waiting for the
/// tab, so this is shorter than the proxy's own `CWD_TENANT_BUDGET`; a pane that
/// misses it loses nothing but the spawn-time copy — the SessionStart route,
/// which waits the full budget, delivers the full body or the UNKNOWN notice.
pub(crate) const SHELL_PANE_TENANT_BUDGET: Duration = Duration::from_secs(3);

/// The tenant a SHELL pane's spawn file composes the policy body for: the
/// coord-mcp proxy's decision for the nonce the identity seam just provisioned
/// for `terminal_id` (Design §6), through the same awaiting composition the
/// SessionStart route uses.
///
/// `TerminalSession::spawn` is synchronous, so this bridges onto the runtime
/// the way the crate's other sync seams do (`block_in_place` + `block_on` on a
/// multi-thread runtime, Tauri's runtime when there is none), bounded by
/// [`SHELL_PANE_TENANT_BUDGET`]. A current-thread runtime cannot be waited on
/// from inside itself, so it composes no body. On a single-bound device the
/// sole tenant is proven and nothing is awaited: the pane composes the default
/// credential's tenant's body, exactly as before.
pub(crate) fn resolve_shell_pane_policy_tenant(terminal_id: &str, workdir: &str) -> Option<Uuid> {
    if Mode::from_env() != Mode::On {
        return None;
    }
    if !crate::coord_mcp::device_is_multi_bound() {
        return spawn_body_tenant(&PolicyTenantDecision::DefaultSlot, default_slot_tenant);
    }
    // The timer is created INSIDE the future, so it is built on whichever
    // runtime polls it — never outside one.
    let bounded = async {
        tokio::time::timeout(SHELL_PANE_TENANT_BUDGET, async {
            let read = read_session_policy_tenant(terminal_id, Some(workdir), None).await;
            policy_tenant_decision(read, true)
        })
        .await
    };
    let decided = match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(bounded))
        }
        Ok(_) => {
            debug!(
                terminal_id,
                "policy-tenant: shell pane spawned on a current-thread runtime — no tenant can be \
                 awaited here, so the pane composes no policy body"
            );
            return None;
        }
        Err(_) => tauri::async_runtime::block_on(bounded),
    };
    match decided {
        Ok(decision) => {
            if let PolicyTenantDecision::Unknown(reason) = &decision {
                debug!(
                    terminal_id,
                    reason = %reason,
                    "policy-tenant: shell pane composes no policy body"
                );
            }
            spawn_body_tenant(&decision, default_slot_tenant)
        }
        // An EXPECTED path on a multi-bound device (a fresh repo's 10 s read
        // outlasts this 3 s budget), and SessionStart delivers the body, so it
        // is not a warning.
        Err(_) => {
            debug!(
                terminal_id,
                budget_secs = SHELL_PANE_TENANT_BUDGET.as_secs(),
                "policy-tenant: the shell pane's session tenant did not settle within the spawn \
                 budget — composing no policy body; SessionStart delivers it"
            );
            None
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests;

/// Test fixtures shared with the Stop-hook readers' tests.
#[cfg(test)]
pub(crate) mod test_support;
