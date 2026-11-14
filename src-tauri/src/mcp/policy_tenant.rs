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

/// The SessionStart route's tenant decision for the session behind
/// `GET /sessions/{route_id}/policy-context`.
///
/// `route_id` is the runner TERMINAL id the hook sends (falling back to the
/// Claude session id); `session_key` is what `mcp::sessions::resolve_session_key`
/// made of it (an `AiCoordRegistrar`-mapped coord session id, else the path id).
/// The registry fallback asks for the terminal's own coord row first — the leg
/// the session-info tenancy block calls the data plane — then for `session_key`.
pub(crate) async fn route_policy_decision(
    route_id: &str,
    session_key: &str,
) -> PolicyTenantDecision {
    let (workdir, registry_session) = route_session_ref(route_id, session_key);
    let read = match tokio::time::timeout(
        ROUTE_TENANT_BUDGET,
        read_session_policy_tenant(route_id, workdir.as_deref(), registry_session),
    )
    .await
    {
        Ok(read) => read,
        Err(_) => PolicyTenantRead::Unnamed(format!(
            "the session's tenant did not settle within the SessionStart route's {}s budget",
            ROUTE_TENANT_BUDGET.as_secs()
        )),
    };
    let multi_bound = device_multi_bound().await;
    policy_tenant_decision(read, multi_bound)
}

/// How long the SessionStart route waits for the session's tenant. The hook
/// calls the route under `curl --max-time 15` and swallows a timeout, so a read
/// that used the proxy's whole `CWD_TENANT_BUDGET` (10 s) before a 4 s fetch
/// would leave under a second of margin and, past it, deliver neither the body
/// nor the fail-open notice. Capping the read keeps the notice deliverable: an
/// unsettled read is `Unnamed`, which the decision turns into UNKNOWN on a
/// multi-bound device and the default slot on a single-bound one.
pub(crate) const ROUTE_TENANT_BUDGET: Duration = Duration::from_secs(8);

/// `(workdir, registry session id)` for the route's session: the hosting
/// terminal's cwd and coord row when `route_id` names a live terminal, else
/// `session_key` read as a registry id.
fn route_session_ref(route_id: &str, session_key: &str) -> (Option<String>, Option<Uuid>) {
    use tauri::Manager;
    let terminal = crate::tauri_app_handle::current().and_then(|app| {
        app.try_state::<std::sync::Arc<crate::terminal::TerminalManager>>()
            .and_then(|tm| tm.get(route_id))
    });
    let workdir = terminal
        .as_ref()
        .map(|t| t.working_dir().trim().to_string())
        .filter(|w| !w.is_empty());
    let registry_session = terminal
        .and_then(|t| t.coord_session_id())
        .or_else(|| Uuid::parse_str(session_key.trim()).ok());
    (workdir, registry_session)
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
}
