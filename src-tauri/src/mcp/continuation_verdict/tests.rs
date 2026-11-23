#![cfg(test)]

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

// ── Status door: the work-status answer -> StatusFetch ──────────────

#[test]
fn only_a_work_status_row_is_reachable_and_everything_else_fails_open() {
    use crate::mcp::session_work_status::OneStatus;
    assert_eq!(
        status_fetch_from_one(OneStatus::Row(Some("finished".into()))),
        reachable(Some("finished"))
    );
    // A row whose work axis is unset is ABSENT — block-eligible.
    assert_eq!(status_fetch_from_one(OneStatus::Row(None)), reachable(None));
    // coord's `unknown` bucket is "not keyed here", not "no status":
    // UNKNOWN, so it allows.
    assert!(matches!(
        status_fetch_from_one(OneStatus::NoRow),
        StatusFetch::Unreachable(_)
    ));
    // A read coord did not settle (401, timeout, bridge column absent)
    // fails OPEN.
    assert!(matches!(
        status_fetch_from_one(OneStatus::Unresolved("HTTP 401".into())),
        StatusFetch::Unreachable(_)
    ));
}

#[test]
fn only_non_blocking_readings_are_cacheable() {
    assert!(is_block_eligible(&reachable(None)));
    assert!(is_block_eligible(&reachable(Some(" Working "))));
    assert!(!is_block_eligible(&reachable(Some("finished"))));
    assert!(!is_block_eligible(&StatusFetch::Unreachable("x".into())));
}

// ── Gate-continuation arming (FlagReading + registry scoping) ───────

#[test]
fn flag_reading_keeps_unset_apart_from_off() {
    assert_eq!(FlagReading::parse(None), FlagReading::Unset);
    assert_eq!(FlagReading::parse(Some("  ")), FlagReading::Unset);
    assert_eq!(
        FlagReading::parse(Some("off")),
        FlagReading::Explicit(Mode::Off)
    );
    assert_eq!(
        FlagReading::parse(Some("bogus")),
        FlagReading::Explicit(Mode::Off)
    );
    assert_eq!(
        FlagReading::parse(Some("On")),
        FlagReading::Explicit(Mode::On)
    );
    assert_eq!(
        FlagReading::parse(Some("observe")),
        FlagReading::Explicit(Mode::Observe)
    );
}

#[test]
fn unset_flag_arms_gate_continuations_only_and_explicit_off_disarms_them() {
    assert_eq!(FlagReading::Unset.mode_for(true), Mode::On);
    assert_eq!(FlagReading::Unset.mode_for(false), Mode::Off);
    assert_eq!(FlagReading::Explicit(Mode::Off).mode_for(true), Mode::Off);
    assert_eq!(FlagReading::Explicit(Mode::On).mode_for(false), Mode::On);
    assert_eq!(
        FlagReading::Explicit(Mode::Observe).mode_for(true),
        Mode::Observe
    );
}

#[test]
fn registry_scoping_blocks_a_gate_continuation_with_no_status() {
    // A terminal the continuation registry holds WITH a gate: an unset
    // flag plus an absent coord status blocks at turn end.
    let gate_tid = "verdict-test-gate";
    let plain_tid = "verdict-test-plain";
    let gateless_tid = "verdict-test-gateless";
    // The registry lookup, injected: only `gate_tid` is a live entry with a
    // gate (a work-unit dispatch's entry has none, so it reads false too).
    let is_gate = |tid: &str| tid == gate_tid;

    let mode = mode_for_terminal_with(FlagReading::Unset, gate_tid, &is_gate);
    assert_eq!(mode, Mode::On);
    assert_eq!(
        decide(mode, false, &reachable(None), false).decision,
        "block"
    );

    // No registry entry: the ordinary dark default — allowed.
    let mode = mode_for_terminal_with(FlagReading::Unset, plain_tid, &is_gate);
    assert_eq!(mode, Mode::Off);
    assert_eq!(
        decide(mode, false, &reachable(None), false).decision,
        "allow"
    );

    // A registry entry WITHOUT a gate (a work-unit dispatch) is not a
    // gate continuation.
    assert_eq!(
        mode_for_terminal_with(FlagReading::Unset, gateless_tid, &is_gate),
        Mode::Off
    );

    // The gate continuation with the flag explicitly `off`: allowed.
    let mode = mode_for_terminal_with(FlagReading::Explicit(Mode::Off), gate_tid, &is_gate);
    assert_eq!(
        decide(mode, false, &reachable(None), false).decision,
        "allow"
    );
}

#[test]
fn rules_url_targets_the_prompt_documents_document() {
    // Coord's DEVICE-authed door:
    // GET /coord/agent-prompt-documents/{kind}/{name} — NOT the removed
    // /coord/policy-documents/continuation-rules.
    let url = continuation_rules_url("https://coord.example.com");
    assert_eq!(
        url,
        "https://coord.example.com/coord/agent-prompt-documents/continuation_rules/session-continuation"
    );
    assert!(
        !url.contains("policy-documents"),
        "must not target the removed legacy surface: {url}"
    );
}

/// The fetch MUST target coord's device/agent door, never the operator
/// `TenantId`-gated one.
///
/// This is a real regression guard, not a tautology: this fetch carries a
/// DEVICE JWT and fail-opens on any error, and `/coord/prompt-documents/*`
/// 403s a device JWT (confirmed against prod). Pointing there is therefore
/// SILENT — no outage, no log line an operator would chase, just every
/// operator edit ignored forever in favour of the built-in default. The
/// path is the only thing distinguishing the two, so it gets pinned.
#[test]
fn rules_url_uses_the_device_authed_door_not_the_operator_one() {
    let url = continuation_rules_url("https://coord.example.com");
    assert!(
        url.contains("/coord/agent-prompt-documents/"),
        "must use the require_jwt door a device JWT can actually read: {url}"
    );
    // The operator route is a strict prefix-free counterpart: assert on the
    // full segment so `agent-prompt-documents` cannot satisfy it.
    assert!(
        !url.contains("/coord/prompt-documents/"),
        "the operator TenantId door 403s this fetch's device JWT, and the \
             fail-open posture would hide that forever: {url}"
    );
}

#[test]
fn rules_parse_coord_prompt_document_shape() {
    // The exact shape coord's `get_one` serializes: a flat
    // `PromptDocumentRow` whose `body` carries the operator's text.
    let coord_doc = json!({
        "id": "6f1c9b3a-0f4e-4a1c-9c2f-3d8a5e7b1c04",
        "tenant_id": "1b7e2c48-5a90-4f3d-8e61-92a4c7d0f5b3",
        "kind": "continuation_rules",
        "name": "session-continuation",
        "description": "Stop-hook continuation umbrella prompt",
        "body": "Operator-edited: finish the follow-ups, then clean up.",
        "format": "markdown",
        "default_source": "prompt_doc/continuation_rules/session-continuation/v1",
        "current_version": 2,
        "updated_by": "josh@qontinui.io",
        "updated_at": "2026-07-17T12:00:00Z"
    });
    // The version rides out with the text: it is what says WHICH revision
    // of the tenant's document a session was told (Design §7).
    assert_eq!(
        rules_from_doc_body(&coord_doc).as_deref(),
        Some("Operator-edited: finish the follow-ups, then clean up."),
        "the body-only reader trust_gate and policy_context share keeps its contract"
    );
    assert_eq!(
        rules_doc_from_body(&coord_doc),
        Some(RulesDoc {
            text: "Operator-edited: finish the follow-ups, then clean up.".to_string(),
            version: Some(2),
        })
    );
}

#[test]
fn rules_parse_direct_and_wrapped_shapes() {
    let direct = json!({"name": "session-continuation", "body": "Do the follow-ups."});
    assert_eq!(
        rules_doc_from_body(&direct),
        Some(RulesDoc {
            text: "Do the follow-ups.".to_string(),
            version: None,
        })
    );
    let wrapped = json!({"document": {"body": "Wrapped text", "current_version": 7}});
    assert_eq!(
        rules_doc_from_body(&wrapped),
        Some(RulesDoc {
            text: "Wrapped text".to_string(),
            version: Some(7),
        })
    );
    assert_eq!(rules_doc_from_body(&json!({"body": "  "})), None);
    assert_eq!(rules_doc_from_body(&json!({})), None);
    assert_eq!(rules_from_doc_body(&json!({})), None);
}

#[test]
fn default_rules_text_is_the_umbrella_prompt() {
    assert!(DEFAULT_CONTINUATION_RULES.contains("follow-ups"));
    assert!(DEFAULT_CONTINUATION_RULES.contains("finished"));
    assert_eq!(RulesDoc::built_in_default().version, None);
}

// ── The session's tenant (plan 2026-11-13-runner-stop-hook-…) ───────

fn tenant_n(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

/// Tests bullet 1 (decision + cache half). The device's default
/// credential claims A; the session is B. The decision targets B's own
/// scope without reading the default, and the rules are cached under
/// (base, B) — a lookup for (base, A) never sees them. Under the old
/// single-slot cache the last fetch was served to every tenant.
#[test]
fn the_decision_targets_bs_own_scope_and_bs_rules_are_cached_under_base_b_only() {
    use crate::mcp::policy_tenant::{fetch_target, policy_tenant_decision, PolicyTenantRead};
    let (a, b) = (tenant_n(0xA1), tenant_n(0xB2));
    let target = fetch_target(
        policy_tenant_decision(
            PolicyTenantRead::ProxyOwned {
                tenant: b,
                credential: true,
            },
            true,
        ),
        || panic!("an Owned decision never reads the default credential's tenant (A)"),
    )
    .expect("B resolved with a usable credential is a target");
    assert_eq!(target.scope, crate::auth::TenantScope::Owned(b));
    assert_eq!(target.tenant, Some(b));

    let base = "https://rules-cache-b-not-a.example";
    let now = Instant::now();
    let b_doc = RulesDoc {
        text: "B's rules".to_string(),
        version: Some(4),
    };
    rules_cache_store(base, target.tenant, now, b_doc.clone());
    assert_eq!(rules_cache_lookup(base, Some(b), now), Some(b_doc));
    assert_eq!(
        rules_cache_lookup(base, Some(a), now),
        None,
        "B's rules are never served to a session of tenant A"
    );
    assert_eq!(rules_cache_lookup(base, None, now), None);
    assert_eq!(
        rules_cache_lookup("https://another-base.example", Some(b), now),
        None
    );
    // And the TTL still applies per slot.
    assert_eq!(
        rules_cache_lookup(base, Some(b), now + CACHE_TTL + Duration::from_secs(1)),
        None
    );
}

/// Tests bullet 1 (request half), and review A7's scope pin. On a runner
/// whose default credential claims A and which holds B's own slot, the
/// rules GET for a session of tenant B presents B's credential — it is
/// built under `target.scope` — and one for the default slot presents A's.
#[test]
fn the_rules_get_for_tenant_b_presents_bs_credential_not_the_defaults() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let _runner = crate::mcp::policy_tenant::test_support::two_tenant_runner(a, b);
    let client = reqwest::Client::new();
    for (scope, tenant) in [
        (crate::auth::TenantScope::Owned(b), b),
        (crate::auth::TenantScope::Device, a),
    ] {
        let request = build_rules_get(
            &client,
            "https://coord.example.com",
            PolicyTarget {
                scope,
                tenant: Some(tenant),
            },
        )
        .expect("each tenant's rules are read under its own credential");
        assert_eq!(
            crate::mcp::policy_tenant::presented_claim(request.headers()),
            Some(tenant),
            "{scope:?}"
        );
        assert_eq!(request.timeout(), Some(&RULES_FETCH_TIMEOUT));
    }
}

/// Tests bullet 4 for the rules GET: no usable slot for the target tenant
/// means the request is refused before it can be sent — never sent
/// unauthenticated, never under the default credential.
#[test]
fn no_rules_request_is_built_for_a_tenant_with_no_usable_credential() {
    let _store = crate::mcp::policy_tenant::test_support::isolated_store();
    let t = Uuid::new_v4();
    let target = PolicyTarget {
        scope: crate::auth::TenantScope::Owned(t),
        tenant: Some(t),
    };
    let refused = build_rules_get(&reqwest::Client::new(), "https://coord.example.com", target)
        .expect_err("an unauthenticated rules read for tenant t must never be built");
    assert!(
        refused.contains(&format!("no usable credential claiming tenant {t}")),
        "{refused}"
    );
}

/// Design §3/§6, review A7. An UNKNOWN tenant reads no tenant's cached
/// rules: with a sentinel cached under the live coord base for the
/// default slot's tenant AND for no tenant, it still gets the built-in
/// default — and its work axis is UNKNOWN (which allows) and writes no
/// status entry.
#[tokio::test]
async fn an_unknown_tenant_reads_no_cached_rules_and_writes_no_status() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let _runner = crate::mcp::policy_tenant::test_support::two_tenant_runner(a, b);
    let base = coord_base();
    let default_tenant = crate::mcp::policy_tenant::default_slot_tenant();
    assert_eq!(
        default_tenant,
        Some(a),
        "the fixture's default slot claims A"
    );
    let sentinel = RulesDoc {
        text: "SENTINEL — an UNKNOWN tenant must never read this".to_string(),
        version: Some(99),
    };
    let now = Instant::now();
    for tenant in [None, default_tenant] {
        rules_cache_store(&base, tenant, now, sentinel.clone());
    }

    let unknown: Result<PolicyTarget, String> = Err("no tenant settled".to_string());
    assert_eq!(
        fetch_continuation_rules(&unknown).await,
        RulesDoc::built_in_default()
    );
    for tenant in [None, default_tenant] {
        assert_eq!(
            rules_cache_lookup(&base, tenant, now),
            Some(sentinel.clone()),
            "an UNKNOWN decision leaves every entry as it found it"
        );
        if let Ok(mut g) = rules_cache().lock() {
            g.remove(&(base.clone(), tenant));
        }
    }

    let status_key = "cv-unknown-tenant-status";
    let status = fetch_session_status(status_key, &unknown).await;
    let StatusFetch::Unreachable(why) = &status else {
        panic!("an unknown tenant's work axis is UNKNOWN, got {status:?}");
    };
    assert!(why.contains("no tenant settled"), "{why}");
    assert!(
        !status_cache()
            .inner
            .lock()
            .unwrap()
            .keys()
            .any(|(_, _, sid)| sid == status_key),
        "an UNKNOWN decision writes no status entry"
    );
    assert_eq!(
        decide(Mode::On, false, &status, false).decision,
        "allow",
        "the existing degraded path: an unknown tenant never blocks"
    );
}

/// Review A4. An ANSWERED tenant decision is reused per (route id, harness
/// session id) for the TTL; an UNKNOWN one is never cached; another
/// session in the same pane does not inherit it.
#[test]
fn an_answered_tenant_decision_is_cached_per_route_and_session_and_unknown_is_not() {
    let b = tenant_n(0xB3);
    let target = PolicyTarget {
        scope: crate::auth::TenantScope::Owned(b),
        tenant: Some(b),
    };
    let now = Instant::now();
    let key: TenantCacheKey = ("cv-term-1".to_string(), "sess-1".to_string());
    tenant_cache_remember(key.clone(), now, &Ok(target));
    assert_eq!(tenant_cache_lookup(&key, now), Some(target));
    assert_eq!(
        tenant_cache_lookup(&("cv-term-1".to_string(), "sess-2".to_string()), now),
        None,
        "the pane's next session asks again"
    );
    assert_eq!(
        tenant_cache_lookup(&key, now + CACHE_TTL + Duration::from_secs(1)),
        None
    );

    let unknown_key: TenantCacheKey = ("cv-term-2".to_string(), "sess-3".to_string());
    tenant_cache_remember(unknown_key.clone(), now, &Err("unknown".to_string()));
    assert_eq!(tenant_cache_lookup(&unknown_key, now), None);
}

/// Review A3. Everything the Stop hook waits for, at its bound, fits
/// inside the hook's own `curl --max-time` — read from the script the
/// runner ships, so the two cannot drift apart unseen.
#[test]
fn the_stop_paths_worst_case_fits_the_hooks_curl_max_time() {
    const HOOK: &str = include_str!("../../../resources/session-restore/claude_stop_hook.sh");
    let max_time = HOOK
        .lines()
        .filter_map(|l| l.split("--max-time").nth(1))
        .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .min()
        .expect("the Stop hook's curl names a --max-time");
    let worst = policy_tenant::STOP_TENANT_BUDGET
        + crate::mcp::session_compliance::TURN_END_BUDGET
        + STATUS_READ_BUDGET
        + policy_tenant::CREDENTIAL_BUILD_TIMEOUT
        + RULES_FETCH_TIMEOUT;
    assert!(
        worst + Duration::from_millis(1500) <= Duration::from_secs(max_time),
        "worst case {worst:?} must leave at least 1.5 s of the hook's --max-time {max_time}s \
         for the route, the transcript read and the connection itself"
    );
    assert!(
        STATUS_READ_BUDGET >= crate::mcp::session_work_status::FETCH_TIMEOUT,
        "the status bound leaves the GET its own timeout"
    );
}

/// Design §7. The verdict response names the tenant, apart from the cause,
/// and the rules version.
#[test]
fn the_verdict_response_carries_the_tenant_and_the_rules_version() {
    let v = VerdictResponse {
        decision: "block",
        prompt: Some("B's rules".to_string()),
        mode: "on",
        reason: "r".to_string(),
        would_block: true,
        session: "s".to_string(),
        status_seen: "absent".to_string(),
        source: "continuation",
        tenant: tenant_n(0xB2).to_string(),
        tenant_reason: None,
        rules_version: Some(4),
    };
    let wire = serde_json::to_value(&v).unwrap();
    assert_eq!(wire["tenant"], json!(tenant_n(0xB2).to_string()));
    assert_eq!(wire["tenant_reason"], Value::Null);
    assert_eq!(wire["rules_version"], json!(4));
    assert_eq!(version_label(Some(4)), "4");
    assert_eq!(version_label(None), "none");
}

// ── Session-compliance nudge delivery ────────────────────────────────
//
// These pin the §A4 invariants that live on THIS side of the seam: the
// hourly cap is inherited (not rebuilt) but namespaced, the nudge is
// independent of `FLAG_ENV`, and the per-session claim is only counted on
// real delivery.

use crate::mcp::session_compliance::{ComplianceNudge, NudgeClass};

/// The tenant label the delivery tests stamp on the verdict.
fn test_tenant() -> TenantLabel {
    ("unknown".to_string(), Some("test".to_string()))
}

fn nudge_for(session: &str) -> ComplianceNudge {
    nudge_of(session, NudgeClass::Report)
}

fn nudge_of(session: &str, class: NudgeClass) -> ComplianceNudge {
    ComplianceNudge {
        session_id: session.to_string(),
        class,
        prompt: "produce the block".to_string(),
        // 1 keeps these cases on the historical once-per-session cap, so
        // they still assert what they were written to assert.
        max_attempts: 1,
    }
}

#[test]
fn no_candidate_means_no_block() {
    assert!(deliver_compliance_nudge("cv-none", None, Mode::On, &test_tenant()).is_none());
}

/// Compliance must not spend the continuation arm's hourly budget.
///
/// Under a shared key a configurable cap could consume all 5 slots and
/// starve session-continuation for an hour — a cross-feature outage caused
/// by an unrelated setting. Exhausting one namespace must leave the other
/// untouched, in both directions.
#[test]
fn compliance_and_continuation_do_not_share_an_hourly_budget() {
    let key = "cv-budget-isolation";
    let now = Instant::now();

    // Drain the CONTINUATION namespace completely.
    for _ in 0..effective_cap() {
        cap_registry().record(key, now);
    }
    assert!(
        cap_registry().would_exceed(key, now, effective_cap()),
        "the continuation budget really is exhausted"
    );

    // Compliance still delivers — different key, untouched budget.
    assert!(
        deliver_compliance_nudge(key, Some(nudge_for(key)), Mode::On, &test_tenant()).is_some(),
        "a drained continuation budget must not suppress the compliance nudge"
    );
}

#[test]
fn a_candidate_blocks_even_when_the_continuation_flag_is_off() {
    // The compliance feature is switched by coord's per-tenant config, NOT
    // by FLAG_ENV — a dark continuation ramp must not disable it.
    let key = "cv-off-mode-still-blocks";
    let v = deliver_compliance_nudge(key, Some(nudge_for(key)), Mode::Off, &test_tenant())
        .expect("compliance blocks under Mode::Off");
    assert_eq!(v.decision, "block");
    assert_eq!(v.source, "compliance");
    assert_eq!(v.mode, "off");
    assert_eq!(v.prompt.as_deref(), Some("produce the block"));
    // Delivery consumed the session's only claim at max_attempts=1.
    assert!(!crate::mcp::session_compliance::mark_nudged(
        key,
        NudgeClass::Report,
        1
    ));
}

#[test]
fn an_exhausted_cap_suppresses_the_nudge_and_leaves_it_eligible() {
    let key = "cv-cap-exhausted";
    let now = Instant::now();
    // Exhaust compliance's OWN namespace in the shared registry — the
    // continuation arm's budget is a different key and no longer gates
    // this path.
    for _ in 0..effective_cap() {
        cap_registry().record(&format!("compliance:{key}"), now);
    }
    assert!(
        deliver_compliance_nudge(key, Some(nudge_for(key)), Mode::On, &test_tenant()).is_none()
    );
    // The per-session claim must NOT have been burned by a nudge the
    // cap swallowed — a later turn is still allowed to deliver it.
    assert!(
        crate::mcp::session_compliance::mark_nudged(key, NudgeClass::Report, 1),
        "cap suppression must leave the session's nudge claim unconsumed"
    );
}

#[test]
fn a_session_already_nudged_is_not_nudged_again() {
    let key = "cv-already-nudged";
    assert!(crate::mcp::session_compliance::mark_nudged(
        key,
        NudgeClass::Report,
        1
    ));
    assert!(
        deliver_compliance_nudge(key, Some(nudge_for(key)), Mode::On, &test_tenant()).is_none()
    );
}

/// THE regression this phase exists to prevent, asserted at the DELIVERY
/// seam: exhausting one class's budget must not suppress the other's.
///
/// Under the old session-only marker key the second delivery here returned
/// `None` — a session that had been nudged about its report could never be
/// told it had skipped the policy pull, and the operator surface read clean
/// while a whole verification arm was dead.
#[test]
fn one_exhausted_nudge_class_does_not_suppress_the_other() {
    let key = "cv-class-independence";

    let first = deliver_compliance_nudge(
        key,
        Some(nudge_of(key, NudgeClass::Report)),
        Mode::On,
        &test_tenant(),
    )
    .expect("the report nudge delivers");
    assert!(
        first.reason.contains("report nudge 1 of 1"),
        "{}",
        first.reason
    );

    // That class is now spent...
    assert!(
        deliver_compliance_nudge(
            key,
            Some(nudge_of(key, NudgeClass::Report)),
            Mode::On,
            &test_tenant()
        )
        .is_none(),
        "the report class is capped at 1"
    );

    // ...and the OTHER class is untouched.
    let second = deliver_compliance_nudge(
        key,
        Some(nudge_of(key, NudgeClass::PolicyPull)),
        Mode::On,
        &test_tenant(),
    )
    .expect("the policy-pull nudge must still deliver");
    assert_eq!(second.decision, "block");
    assert!(
        second.reason.contains("policy_pull nudge 1 of 1"),
        "the reason must name the class, so two budgets never read as one: {}",
        second.reason
    );
}
