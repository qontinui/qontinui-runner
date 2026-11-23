#![cfg(test)]

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
    let _store = super::test_support::isolated_store();
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
async fn an_unsettled_read_within_the_stop_budget_is_unknown_on_multi_bound_and_default_on_single()
{
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
    assert!(started.elapsed() <= STOP_TENANT_BUDGET, "never late");

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

// ── live_read_then_recorded: only the live leg is bounded (review round 2 A3) ──

/// A live read that never answers must not hide the record's durable tenant:
/// the live leg times out on its own (leaving the fallback room inside the
/// decision budget), the recorded tenant with a usable credential decides
/// `Owned`, and the binding count is never read.
#[tokio::test(start_paused = true)]
async fn a_pending_live_read_still_lets_the_recorded_tenant_decide() {
    let t = tenant(0x7C);
    let decided = decide_target_within(
        ROUTE_TENANT_BUDGET,
        live_read_then_recorded(
            ROUTE_TENANT_BUDGET.saturating_sub(DECISION_RESERVE * 2),
            std::future::pending(),
            Some(t),
            |_| async { Some(true) },
        ),
        must_not_poll("the binding count"),
        must_not_poll("the default credential"),
    )
    .await;
    assert_eq!(
        decided,
        Ok(PolicyTarget {
            scope: TenantScope::Owned(t),
            tenant: Some(t),
        })
    );
    // Without a usable credential the same record is UNKNOWN.
    let decided = decide_target_within(
        ROUTE_TENANT_BUDGET,
        live_read_then_recorded(
            ROUTE_TENANT_BUDGET.saturating_sub(DECISION_RESERVE * 2),
            std::future::pending(),
            Some(t),
            |_| async { Some(false) },
        ),
        must_not_poll("the binding count"),
        must_not_poll("the default credential"),
    )
    .await;
    assert!(decided.is_err(), "{decided:?}");
}

/// Review round 3 A1. On a multi-bound device a record stamped with the
/// DEFAULT binding's tenant may be the machine-row fall-through of an unsettled
/// spawn, so it never stands in for the live read: a pending live read plus
/// that stamp is UNKNOWN (withheld), never `Owned(default)`. Any other stamp,
/// or the same stamp on a single-bound device, still decides.
#[tokio::test(start_paused = true)]
async fn a_default_tenant_stamp_on_a_multi_bound_device_is_unknown_not_the_default() {
    let (default, other) = (tenant(0xD0), tenant(0xD1));
    let admit = move |t: Uuid| async move {
        recorded_tenant_admissible(t, Some(default), true).then_some(true)
    };
    let decided = decide_target_within(
        ROUTE_TENANT_BUDGET,
        live_read_then_recorded(
            ROUTE_TENANT_BUDGET.saturating_sub(DECISION_RESERVE * 2),
            std::future::pending(),
            Some(default),
            admit,
        ),
        async { true },
        must_not_poll("the default credential"),
    )
    .await;
    assert!(
        decided.is_err(),
        "a default-tenant stamp on a multi-bound device is withheld, got {decided:?}"
    );

    assert!(!recorded_tenant_admissible(default, Some(default), true));
    assert!(recorded_tenant_admissible(other, Some(default), true));
    assert!(recorded_tenant_admissible(default, Some(default), false));
    assert!(recorded_tenant_admissible(default, None, true));
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
    let (tenant, reason) = target_label(&fetch_target(PolicyTenantDecision::DefaultSlot, || None));
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
