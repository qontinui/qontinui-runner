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
    let read = async {
        let live =
            read_session_policy_tenant(terminal_id, workdir.as_deref(), registry_session).await;
        let Some(tenant) = recorded_fallback_tenant(&live, recorded_tenant) else {
            return live;
        };
        let credential =
            qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                crate::auth::device_holds_usable_binding(&tenant)
            })
            .await
            .unwrap_or(false);
        PolicyTenantRead::Registry { tenant, credential }
    };
    decide_target_within(
        budget,
        read,
        device_multi_bound(),
        default_slot_tenant_off_runtime(),
    )
    .await
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
mod tests {
    use super::*;

    fn tenant(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    /// Every read that can reach the decision, for the exhaustive checks.
    fn unresolvable_reads() -> Vec<PolicyTenantRead> {
        vec![
            PolicyTenantRead::Unnamed(
                "no coord-mcp session key names this session (no_session_nonce), and the \
                 session registry holds no tenant for it"
                    .to_string(),
            ),
            PolicyTenantRead::ProxyRefused {
                code: "COORD_MCP_PROXY_TENANT_UNRESOLVABLE",
                message: "tenant unresolvable by any route".to_string(),
            },
            PolicyTenantRead::ProxyRefused {
                code: crate::coord_mcp::TENANT_REPO_UNSETTLED_CODE,
                message: "the tenant of this session's repo is not established yet".to_string(),
            },
        ]
    }

    // ── new_session_body_tenant: a machine-row answer never briefs a multi-bound spawn ──

    /// Review finding A1: on a multi-bound device an UNSETTLED repo answer
    /// falls through to the machine rows (the device default), so a direct-exec
    /// spawn must compose no body rather than the default tenant's. A settled
    /// answer, or any single-bound device, keeps the stated tenant.
    #[test]
    fn a_direct_exec_spawn_composes_no_body_on_an_unsettled_repo_of_a_multi_bound_device() {
        let a = tenant(0xA);
        assert_eq!(new_session_body_tenant(Some(a), false, true), None);
        assert_eq!(new_session_body_tenant(Some(a), true, true), Some(a));
        assert_eq!(new_session_body_tenant(Some(a), false, false), Some(a));
        assert_eq!(new_session_body_tenant(Some(a), true, false), Some(a));
        assert_eq!(new_session_body_tenant(None, true, true), None);
    }

    // ── spawn_policy_body_in: the session's tenant, never the default's ──

    /// Design §6 / Tests bullet 1. The device's default credential claims A
    /// (A's cache file is the one the pre-fix `spawn_policy_body()` read, for
    /// `tenant_of_jwt(AuthManager::get_access_token())`); the session's tenant
    /// is B. The spawn body must be B's.
    #[test]
    fn spawn_body_for_session_tenant_b_is_bs_file_when_the_default_claims_a() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        crate::session::spawn_prompt::write_policy_body_cache(dir.path(), &a, "A's policy body")
            .expect("write A");
        crate::session::spawn_prompt::write_policy_body_cache(dir.path(), &b, "B's policy body")
            .expect("write B");

        assert_eq!(
            spawn_policy_body_in(dir.path(), Mode::On, Some(b)).as_deref(),
            Some("B's policy body"),
            "a session of tenant B must be composed B's body, never the default (A)'s"
        );
        assert_eq!(
            spawn_policy_body_in(dir.path(), Mode::On, Some(a)).as_deref(),
            Some("A's policy body")
        );
        // No tenant composes nothing — there is no unscoped fallback name.
        assert_eq!(spawn_policy_body_in(dir.path(), Mode::On, None), None);
        // A tenant with no cache yet composes nothing (inline briefing).
        assert_eq!(
            spawn_policy_body_in(dir.path(), Mode::On, Some(tenant(0xC3))),
            None
        );
        // The Mode gate is unchanged: off/observe inject nothing.
        for dark in [Mode::Off, Mode::Observe] {
            assert_eq!(spawn_policy_body_in(dir.path(), dark, Some(b)), None);
        }
    }

    // ── policy_tenant_decision ──────────────────────────────────────────

    /// Tests bullet 3. A multi-bound device with an unresolvable session tenant
    /// is UNKNOWN, never the default slot — and the render is the notice,
    /// never A's documents.
    #[test]
    fn multi_bound_with_an_unresolvable_tenant_is_unknown_and_renders_the_notice() {
        for read in unresolvable_reads() {
            let decision = policy_tenant_decision(read.clone(), true);
            let PolicyTenantDecision::Unknown(reason) = &decision else {
                panic!("{read:?} on a multi-bound device must be Unknown, got {decision:?}");
            };
            assert!(
                reason.contains("UNKNOWN on a device bound to several tenants"),
                "{reason}"
            );
            assert!(
                reason.contains("did not inject the device default tenant's policy"),
                "{reason}"
            );
            // No target ⇒ no fetch.
            assert_eq!(
                fetch_target(decision.clone(), || panic!(
                    "an Unknown never reads the default"
                )),
                Err(reason.clone())
            );
            assert_eq!(
                spawn_body_tenant(&decision, || panic!("an Unknown never reads the default")),
                None
            );
            // What the route injects for it: the notice, naming the cause, and
            // no policy body at all.
            let notice = super::super::policy_context::render_failure_notice(
                reason,
                "startup",
                "2026-11-13T12:00:00Z",
            );
            assert!(notice.contains("POLICY PULL FAILED: this session's tenant is UNKNOWN"));
            assert!(!notice.contains("===== policy/session-protocol"));
        }
    }

    /// Tests bullet 4: the first-call race. A repo still unsettled after the
    /// proxy's budget on a multi-bound device is UNKNOWN, and the cause names
    /// the proxy's retryable code.
    #[test]
    fn a_repo_unsettled_refusal_on_a_multi_bound_device_is_unknown() {
        let decision = policy_tenant_decision(
            PolicyTenantRead::ProxyRefused {
                code: crate::coord_mcp::TENANT_REPO_UNSETTLED_CODE,
                message: "the tenant of this session's repo is not established yet".to_string(),
            },
            true,
        );
        let PolicyTenantDecision::Unknown(reason) = decision else {
            panic!("a repo-unsettled refusal must never resolve to the default: {decision:?}");
        };
        assert!(
            reason.contains(crate::coord_mcp::TENANT_REPO_UNSETTLED_CODE),
            "{reason}"
        );
    }

    /// Tests bullet 5 / Design §2a. The proxy (or the registry) names tenant T
    /// and this runner holds no usable credential for T: UNKNOWN on every
    /// device, and there is no target, so no fetch is made.
    #[test]
    fn a_resolved_tenant_with_no_usable_credential_is_unknown_and_fetches_nothing() {
        let t = tenant(0x5A);
        for read in [
            PolicyTenantRead::ProxyOwned {
                tenant: t,
                credential: false,
            },
            PolicyTenantRead::Registry {
                tenant: t,
                credential: false,
            },
        ] {
            for multi_bound in [true, false] {
                let decision = policy_tenant_decision(read.clone(), multi_bound);
                let PolicyTenantDecision::Unknown(reason) = &decision else {
                    panic!("{read:?} (multi_bound={multi_bound}) must be Unknown: {decision:?}");
                };
                assert!(
                    reason.contains(&format!("no usable credential for tenant {t}")),
                    "the notice names the tenant it could not act as: {reason}"
                );
                assert!(
                    fetch_target(decision.clone(), || panic!("never read")).is_err(),
                    "no target, so nothing is fetched"
                );
            }
        }
    }

    /// A resolved tenant with a usable credential is fetched under ITS scope and
    /// filed under it — on either device shape.
    #[test]
    fn a_resolved_tenant_with_a_credential_is_owned_and_fetched_under_its_own_scope() {
        let b = tenant(0xB2);
        for read in [
            PolicyTenantRead::ProxyOwned {
                tenant: b,
                credential: true,
            },
            PolicyTenantRead::Registry {
                tenant: b,
                credential: true,
            },
        ] {
            for multi_bound in [true, false] {
                let decision = policy_tenant_decision(read.clone(), multi_bound);
                assert_eq!(decision, PolicyTenantDecision::Owned(b), "{read:?}");
                assert_eq!(
                    fetch_target(decision.clone(), || panic!("Owned never reads the default")),
                    Ok(PolicyTarget {
                        scope: TenantScope::Owned(b),
                        tenant: Some(b),
                    })
                );
                assert_eq!(
                    spawn_body_tenant(&decision, || panic!("Owned never reads the default")),
                    Some(b)
                );
            }
        }
    }

    /// Tests bullet 9 (decision half). On a single-bound device the sole tenant
    /// is proven: every inconclusive read falls back to the default slot, which
    /// is exactly what the pre-fix code fetched under.
    #[test]
    fn a_single_bound_device_falls_back_to_the_default_slot_as_before() {
        let a = tenant(0xA1);
        for read in unresolvable_reads()
            .into_iter()
            .chain([PolicyTenantRead::ProxyDefaultSlot])
        {
            let decision = policy_tenant_decision(read.clone(), false);
            assert_eq!(decision, PolicyTenantDecision::DefaultSlot, "{read:?}");
            assert_eq!(
                fetch_target(decision.clone(), || Some(a)),
                Ok(PolicyTarget {
                    scope: TenantScope::Device,
                    tenant: Some(a),
                })
            );
            assert_eq!(spawn_body_tenant(&decision, || Some(a)), Some(a));
        }
    }

    /// The proxy answering the default slot is a DECISION, not an inconclusive
    /// read: the session's own coord-mcp calls present the default credential,
    /// so the default slot's policy is the matching one even on a multi-bound
    /// device.
    #[test]
    fn the_proxy_answering_the_default_slot_is_the_default_slot_on_any_device() {
        for multi_bound in [true, false] {
            assert_eq!(
                policy_tenant_decision(PolicyTenantRead::ProxyDefaultSlot, multi_bound),
                PolicyTenantDecision::DefaultSlot
            );
        }
        // A default credential naming no tenant targets no tenant: nothing is
        // cached or filed under a guessed one.
        assert_eq!(
            fetch_target(PolicyTenantDecision::DefaultSlot, || None),
            Ok(PolicyTarget {
                scope: TenantScope::Device,
                tenant: None,
            })
        );
    }

    // ── cache_file_tenant ───────────────────────────────────────────────

    /// Tests bullet 6 (pure half). The file is named for the PRESENTED bearer's
    /// claim, and only when it is the session's tenant.
    #[test]
    fn the_cache_file_is_named_for_the_presented_claim_only_when_it_is_the_sessions() {
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        assert_eq!(cache_file_tenant(Some(b), Some(b)), Some(b));
        assert_eq!(
            cache_file_tenant(Some(b), Some(a)),
            None,
            "a body fetched under A's credential is never filed under B"
        );
        assert_eq!(cache_file_tenant(Some(b), None), None, "no claim, no file");
        assert_eq!(
            cache_file_tenant(None, Some(a)),
            None,
            "no session tenant, no file"
        );
        assert_eq!(cache_file_tenant(None, None), None);
    }

    // ── The presented credential (moved from policy_context) ────────────

    /// A JWT-shaped token carrying `tenant_id` — the same shape
    /// `tenant_of_jwt_reads_the_claim_and_rejects_non_uuids` builds.
    pub(crate) fn jwt_claiming(tenant: Uuid) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&serde_json::json!({"tenant_id": tenant.to_string()})).unwrap()
            )
        )
    }

    #[test]
    fn tenant_of_jwt_reads_the_claim_and_rejects_non_uuids() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let tenant = Uuid::new_v4();
        let jwt_with = |claims: serde_json::Value| {
            format!(
                "h.{}.s",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
            )
        };
        assert_eq!(
            tenant_of_jwt(&jwt_with(
                serde_json::json!({"tenant_id": tenant.to_string()})
            )),
            Some(tenant)
        );
        assert_eq!(
            tenant_of_jwt(&jwt_with(serde_json::json!({"tenant_id": "acme"}))),
            None
        );
        assert_eq!(tenant_of_jwt(&jwt_with(serde_json::json!({}))), None);
        assert_eq!(tenant_of_jwt("opaque-token"), None);
    }

    /// The claim is read off the request's own `Authorization` header — the
    /// bearer that will actually be presented — and nothing else.
    #[test]
    fn the_presented_claim_is_read_off_the_requests_own_bearer() {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
        let t = Uuid::new_v4();
        let mut headers = HeaderMap::new();
        assert_eq!(presented_claim(&headers), None, "no header, no claim");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", jwt_claiming(t))).unwrap(),
        );
        assert_eq!(presented_claim(&headers), Some(t));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert_eq!(presented_claim(&headers), None, "a non-bearer credential");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque-token"),
        );
        assert_eq!(presented_claim(&headers), None, "a bearer naming no tenant");
    }

    /// The last-moment guard: a request is sent only when its credential
    /// claims the target tenant. A target naming no tenant asserts nothing.
    #[test]
    fn a_request_is_sent_only_under_the_target_tenants_own_credential() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(check_presented(Some(b), Some(b)), Ok(()));
        let wrong = check_presented(Some(b), Some(a)).expect_err("A's credential for B");
        assert!(wrong.contains(&format!("claims tenant {a}")), "{wrong}");
        let none = check_presented(Some(b), None).expect_err("no credential for B");
        assert!(
            none.contains(&format!("no usable credential claiming tenant {b}")),
            "{none}"
        );
        assert_eq!(check_presented(None, None), Ok(()));
        assert_eq!(check_presented(None, Some(a)), Ok(()));
    }

    // ── build_checked: the guard on the request that would go out ───────

    /// Stop-hook Tests bullet 1/3 at the request layer: on a device whose
    /// default credential claims A, a request for a session of tenant B goes
    /// out only when the bytes on the wire claim B. A request carrying A's
    /// credential for B is refused before it is sent.
    #[test]
    fn build_checked_sends_tenant_bs_request_only_under_bs_credential() {
        let client = reqwest::Client::new();
        let (a, b) = (tenant(0xA1), tenant(0xB2));
        let target_b = fetch_target(PolicyTenantDecision::Owned(b), || Some(a)).unwrap();
        let url = "https://coord.example.com/coord/session-compliance/config";

        let (_, presented) = build_checked(client.get(url).bearer_auth(jwt_claiming(b)), target_b)
            .expect("B's credential for B is sent");
        assert_eq!(presented, Some(b));

        let refused = build_checked(client.get(url).bearer_auth(jwt_claiming(a)), target_b)
            .expect_err("A's credential is never presented for B");
        assert!(refused.contains(&format!("claims tenant {a}")), "{refused}");
    }

    /// The unpaired case on the tenant-stating seam: a default slot whose
    /// credential claims no tenant asserts nothing for `check_presented`, so
    /// `build_checked` itself refuses a request that carries no bearer at all.
    /// An untenanted bearer is still sent (it is the default credential).
    #[test]
    fn build_checked_refuses_a_request_that_presents_no_credential_at_all() {
        let client = reqwest::Client::new();
        let target = fetch_target(PolicyTenantDecision::DefaultSlot, || None).unwrap();
        let url = "https://coord.example.com/coord/sessions/x/compliance";
        let refused = build_checked(client.post(url), target)
            .expect_err("an unauthenticated request is never sent");
        assert!(refused.contains("no usable device credential"), "{refused}");
        let (_, presented) = build_checked(client.post(url).bearer_auth("opaque"), target)
            .expect("an untenanted default credential is still the default credential");
        assert_eq!(presented, None);
    }

    /// Stop-hook Tests bullet 4: for a tenant this runner holds no usable
    /// credential for, `coord_get_for(Owned(t))` / `coord_post_for(Owned(t))`
    /// would go out UNAUTHENTICATED — and `build_checked` refuses both. `t` is
    /// a fresh random uuid, so no slot can exist for it in whatever store the
    /// test process reads.
    #[test]
    fn no_stop_hook_request_is_built_for_a_tenant_with_no_usable_credential() {
        let client = reqwest::Client::new();
        let t = Uuid::new_v4();
        let target = fetch_target(PolicyTenantDecision::Owned(t), || {
            panic!("Owned never reads the default")
        })
        .unwrap();
        let url = "https://coord.example.com/coord/session-compliance/config";
        for rb in [
            crate::coord_http::coord_get_for(&client, url, target.scope),
            crate::coord_http::coord_post_for(&client, url, target.scope),
        ] {
            let refused = build_checked(rb, target)
                .expect_err("an unauthenticated request for tenant t must never be built to send");
            assert!(
                refused.contains(&format!("no usable credential claiming tenant {t}")),
                "{refused}"
            );
        }
    }

    // ── decide_target_within: one deadline for the whole decision ──────

    /// A read the decision must never make: panics if polled.
    async fn must_not_poll<T>(what: &'static str) -> T {
        panic!("{what} must not be read on this arm")
    }

    /// Stop-hook Tests bullet 6. A tenant read that has not settled is
    /// `Unnamed` once the read's share of the budget is spent: UNKNOWN on a
    /// multi-bound device (no target, so no Stop-hook request) and the default
    /// slot on a single-bound one — and the whole decision still finishes
    /// inside `STOP_TENANT_BUDGET`. The clock is paused, so the budget elapses
    /// without waiting for it.
    #[tokio::test(start_paused = true)]
    async fn an_unsettled_read_within_the_stop_budget_is_unknown_on_multi_bound_and_default_on_single(
    ) {
        let a = tenant(0xA1);
        let started = tokio::time::Instant::now();
        let multi = decide_target_within(
            STOP_TENANT_BUDGET,
            std::future::pending(),
            async { true },
            must_not_poll("the default credential (an Unknown never reads it)"),
        )
        .await;
        let Err(reason) = &multi else {
            panic!("an unsettled read on a multi-bound device must be UNKNOWN, got {multi:?}");
        };
        assert!(reason.contains("ms budget"), "{reason}");
        assert!(started.elapsed() <= STOP_TENANT_BUDGET);

        let single = decide_target_within(
            STOP_TENANT_BUDGET,
            std::future::pending(),
            async { false },
            async move { Some(a) },
        )
        .await;
        assert_eq!(
            single,
            Ok(PolicyTarget {
                scope: TenantScope::Device,
                tenant: Some(a),
            })
        );

        // A read that answers inside the budget is passed through untouched.
        let settled = PolicyTenantRead::ProxyOwned {
            tenant: a,
            credential: true,
        };
        let s = settled.clone();
        assert_eq!(
            read_within(STOP_TENANT_BUDGET, async move { s }).await,
            settled
        );
    }

    /// Review A3: the binding count and the default credential's claim run
    /// INSIDE the one deadline. A decision whose binding count never settles
    /// is UNKNOWN by the budget, never the default slot and never late.
    #[tokio::test(start_paused = true)]
    async fn a_decision_whose_binding_count_never_settles_is_unknown_within_the_budget() {
        let started = tokio::time::Instant::now();
        let decided = decide_target_within(
            STOP_TENANT_BUDGET,
            async { PolicyTenantRead::Unnamed("no nonce".into()) },
            std::future::pending(),
            must_not_poll("the default credential (never reached)"),
        )
        .await;
        let Err(reason) = &decided else {
            panic!("an uncounted device must be UNKNOWN, got {decided:?}");
        };
        assert!(
            reason.contains("did not fall back to the device default"),
            "{reason}"
        );
        assert_eq!(started.elapsed(), STOP_TENANT_BUDGET);

        let decided = decide_target_within(
            STOP_TENANT_BUDGET,
            async { PolicyTenantRead::ProxyDefaultSlot },
            async { false },
            std::future::pending(),
        )
        .await;
        assert!(
            decided.is_err(),
            "a default claim that never reads is UNKNOWN too"
        );
    }

    /// Review A4: a read that decides on its own never pays for the binding
    /// count — the count's future is not polled at all.
    #[tokio::test]
    async fn a_read_that_decides_alone_skips_the_binding_count() {
        let b = tenant(0xB2);
        for read in [
            PolicyTenantRead::ProxyOwned {
                tenant: b,
                credential: true,
            },
            PolicyTenantRead::Registry {
                tenant: b,
                credential: false,
            },
            PolicyTenantRead::ProxyDefaultSlot,
        ] {
            assert!(!needs_binding_count(&read), "{read:?}");
            let _ = decide_target_within(
                STOP_TENANT_BUDGET,
                async move { read },
                must_not_poll("the binding count"),
                async { None },
            )
            .await;
        }
        for read in unresolvable_reads() {
            assert!(needs_binding_count(&read), "{read:?}");
        }
    }

    /// The Stop path waits less than SessionStart: it runs on every turn end,
    /// under a hook that allows the whole endpoint 10 s. The reserve leaves the
    /// read most of the budget.
    #[test]
    fn the_stop_budget_is_below_the_sessionstart_budget() {
        assert!(STOP_TENANT_BUDGET < ROUTE_TENANT_BUDGET);
        assert!(DECISION_RESERVE < STOP_TENANT_BUDGET / 2);
    }

    // ── recorded_fallback_tenant: the record's durable tenant (review A5) ──

    /// After a runner restart the registry knows nothing about a session the
    /// boot sweep closes, so the live read is `Unnamed`; the record's durable
    /// tenant then decides — `Owned` with a usable credential, UNKNOWN without
    /// one (on every device). A live answer always outranks the stamp.
    #[test]
    fn a_closed_records_durable_tenant_decides_when_the_live_read_named_none() {
        let (t, other) = (tenant(0x7A), tenant(0x7B));
        let unnamed = PolicyTenantRead::Unnamed("no nonce, no registry row".into());
        assert_eq!(recorded_fallback_tenant(&unnamed, Some(t)), Some(t));
        assert_eq!(recorded_fallback_tenant(&unnamed, None), None);
        for live in [
            PolicyTenantRead::ProxyOwned {
                tenant: other,
                credential: true,
            },
            PolicyTenantRead::ProxyDefaultSlot,
            PolicyTenantRead::ProxyRefused {
                code: "C",
                message: "m".into(),
            },
            PolicyTenantRead::Registry {
                tenant: other,
                credential: true,
            },
        ] {
            assert_eq!(recorded_fallback_tenant(&live, Some(t)), None, "{live:?}");
        }
        for multi_bound in [true, false] {
            assert_eq!(
                policy_tenant_decision(
                    PolicyTenantRead::Registry {
                        tenant: t,
                        credential: true
                    },
                    multi_bound
                ),
                PolicyTenantDecision::Owned(t)
            );
            assert!(matches!(
                policy_tenant_decision(
                    PolicyTenantRead::Registry {
                        tenant: t,
                        credential: false
                    },
                    multi_bound
                ),
                PolicyTenantDecision::Unknown(_)
            ));
        }
    }

    // ── close_session_ref: the close-time inputs (Design §5) ────────────

    /// Stop-hook Tests bullet 5 (pure half). The close-time decision reads
    /// the RECORD: its own working dir first, and the coord row keyed by the
    /// session's own harness id, then the row the record names, and only then
    /// the terminal's live row — which by close may belong to the pane's next
    /// session.
    #[test]
    fn the_close_time_inputs_come_from_the_record_before_the_live_terminal() {
        let (own, recorded, live) = (tenant(1), tenant(2), tenant(3));
        assert_eq!(
            close_session_ref(
                Some(" /w/rec "),
                Some("/w/live"),
                Some(own),
                Some(recorded),
                Some(live)
            ),
            (Some("/w/rec".to_string()), Some(own))
        );
        assert_eq!(
            close_session_ref(None, Some("/w/live"), None, Some(recorded), Some(live)),
            (Some("/w/live".to_string()), Some(recorded))
        );
        assert_eq!(
            close_session_ref(Some("  "), None, None, None, Some(live)),
            (None, Some(live))
        );
        assert_eq!(
            close_session_ref(None, None, None, None, None),
            (None, None)
        );
    }

    // ── target_label ────────────────────────────────────────────────────

    #[test]
    fn the_target_label_names_the_tenant_and_carries_the_cause_apart() {
        let b = tenant(0xB2);
        assert_eq!(
            target_label(&fetch_target(PolicyTenantDecision::Owned(b), || None)),
            (b.to_string(), None)
        );
        let (tenant, reason) =
            target_label(&fetch_target(PolicyTenantDecision::DefaultSlot, || None));
        assert_eq!(tenant, "default-slot");
        assert!(reason.is_some());
        assert_eq!(
            target_label(&fetch_target(
                PolicyTenantDecision::Unknown("no nonce".into()),
                || None
            )),
            ("unknown".to_string(), Some("no nonce".to_string()))
        );
    }

    // ── The two-tenant runner fixture, end to end ───────────────────────

    /// The fixture is what the module tests in `continuation_verdict`,
    /// `session_compliance` and `session_work_status` build on: a request
    /// built under `Owned(B)` presents B's credential and one under `Device`
    /// presents the default (A's) — read off the request's own header.
    #[test]
    fn the_two_tenant_runner_presents_each_scopes_own_credential() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let _runner = test_support::two_tenant_runner(a, b);
        let client = reqwest::Client::new();
        let url = "https://coord.example.com/coord/x";
        for (scope, tenant) in [
            (TenantScope::Owned(b), Some(b)),
            (TenantScope::Device, Some(a)),
        ] {
            let target = PolicyTarget { scope, tenant };
            let (_, presented) = build_checked(
                crate::coord_http::coord_get_for(&client, url, scope),
                target,
            )
            .expect("each scope presents its own tenant's credential");
            assert_eq!(presented, tenant, "{scope:?}");
        }
    }
}

/// Test fixtures shared with the Stop-hook readers' tests.
#[cfg(test)]
pub(crate) mod test_support {
    use uuid::Uuid;

    /// A device JWT claiming `tenant`, unexpired for the life of the test
    /// process (the slot selector refuses an expired one). One string per
    /// tenant per process, so "the request presented the token we stored" is
    /// a real comparison.
    pub(crate) fn device_jwt(tenant: Uuid) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        static EXP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let exp = *EXP.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_secs()
                + 3_600
        });
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({"sub_type": "device", "tenant_id": tenant, "exp": exp}).to_string(),
        );
        format!("{header}.{payload}.sig")
    }

    /// A runner paired for `a` (the default binding: the legacy slot, and by
    /// the mirror its own slot) that also holds `b`'s own slot — inside an
    /// isolated ambient, so every credential read lands in a throwaway store and
    /// never the OS keychain. Hold the returned guard for the test's life; it
    /// also holds the process-wide env lock.
    pub(crate) fn two_tenant_runner(
        a: Uuid,
        b: Uuid,
    ) -> qontinui_runner_lib::ambient::test_support::IsolatedAmbient {
        let ambient = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        let am = crate::auth::AuthManager::new();
        am.store_tokens(&device_jwt(a), "").expect("store A");
        am.store_tenant_device_jwt(&b, &device_jwt(b))
            .expect("store B");
        ambient
    }
}
