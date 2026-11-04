//! CUTOVER-ONLY: see the `mod` declaration in `mcp_api.rs`. Drop with it.

use super::{
    coord_mcp_tool_policy_json, COORD_MCP_ALLOWED_METHODS, COORD_MCP_ALLOWED_TOOLS,
    COORD_MCP_ALLOWED_TOOL_PREFIXES, COORD_MCP_DELIBERATE_EXCLUSIONS,
    COORD_MCP_DELIBERATE_EXCLUSION_DOORS, COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES,
};

const OLD_COORD_MCP_ALLOWED_METHODS: &[&str] = &[
    "initialize",
    "notifications/cancelled",
    "notifications/initialized",
    "notifications/progress",
    "ping",
    "tools/call",
    "tools/list",
];
const OLD_COORD_MCP_ALLOWED_TOOLS: &[&str] = &[
    "coord_ack_message",
    "coord_adopt_pr",
    "coord_agent_registry_effective",
    "coord_alert_claim",
    "coord_alert_queue",
    "coord_alert_release",
    "coord_am_i_clear",
    "coord_answer_agent_question",
    "coord_ask_question",
    "coord_attest_gate",
    // Plan 2026-10-17-per-repo-tenant-binding-overrides-the-org-tenant Phase 6:
    // the per-repo tenant binding door, forwarded ahead of coord serving it.
    // coord will authorize it (the device user must be admin in BOTH tenants);
    // this list only forwards. Risk and limits: the doc comment above.
    "coord_bind_repo_tenant",
    "coord_bind_self_session",
    "coord_blockers",
    "coord_build_info",
    "coord_can",
    "coord_cancel_continuation",
    "coord_change_conflict",
    "coord_check_gate_predicate",
    "coord_check_install_safety",
    "coord_check_publish_safety",
    "coord_check_repo_name",
    "coord_citations_reenrich",
    "coord_claim_acquire",
    "coord_claim_check",
    "coord_claim_heartbeat",
    "coord_claim_release",
    "coord_conflict_check",
    "coord_correct_gate",
    "coord_create_repo",
    "coord_declare_intent",
    // The direct supersession-declaration door (qontinui-coord, operator
    // directive 2026-10-03, served policy `git-operations`
    // `a-landed-adoption-closes-its-predecessor`). It records the declaration
    // `coord_repoint_gate`'s supersession arm needs when a successor's title
    // token was missing or not recorded — every precondition (coord knows the
    // predecessor, tenant owns both repos, successor LANDED and not a fork,
    // successor's text references the predecessor, author trusted or a repo
    // writer, predecessor not landed) is verified by coord. Withheld here it
    // would answer `-32601`, and only the gate's registrant could move it. It
    // never authorizes a coord CLOSE of the predecessor: the
    // closer skips agent-declared rows and acts only on a title declaration.
    "coord_declare_supersession",
    "coord_diagnose",
    "coord_diff_impact",
    "coord_edit_predict",
    "coord_edit_predict_and_check",
    "coord_edit_verify",
    "coord_expectation_checkin",
    "coord_expectation_close",
    "coord_expectation_register",
    // Plan 2026-10-13-coord-expedited-land-lever-for-agents Phase 3b: the
    // READ-BACK of an expedited-land request. A read — it reports a request's
    // state, refusal and land sha, and lands nothing. Its write twin,
    // `coord_request_expedited_land`, is a DELIBERATE exclusion (below).
    "coord_expedited_land_status",
    "coord_explain_isolation_decision",
    "coord_explain_pr_close",
    "coord_explain_ref_event",
    "coord_explain_worktree",
    "coord_find_references",
    "coord_fixer_arm_readiness",
    "coord_fleet_dispatch_roles",
    "coord_fleet_drain_status",
    "coord_force_clear_gate",
    "coord_gate_doctor",
    "coord_gate_inspect",
    "coord_gate_list",
    "coord_gate_status",
    "coord_get_answer",
    "coord_get_prompt_document",
    "coord_inbox",
    "coord_is_commit_live",
    "coord_is_merge_safe",
    "coord_land_provenance_backfill",
    "coord_layering_triage",
    "coord_list_prompt_documents",
    "coord_list_worktrees",
    // Plan 2026-09-17-findings-carry-a-triage-stamp-and-the-steward-reads-since-last-run
    // Phase 2: the triage-stamp door. coord's `agent_tool_access::DEVICE_DEFAULT_TOOLS`
    // is the grant authority; this list only forwards.
    "coord_mark_findings_triaged",
    // Plan 2026-09-21-a-memory-write-receipt-cannot-be-read-back-by-any-door-a-degraded-session-holds
    // Phase 1: the by-id read-back of a memory write receipt. A pure read; added
    // WITH the coord tool so the receipt's own door never answers -32601.
    "coord_memory_get",
    "coord_memory_overview",
    "coord_memory_record",
    "coord_memory_search",
    "coord_memory_supersede",
    "coord_merge_order",
    // Plan 2026-09-17-a-sender-cannot-see-its-ack-and-recon-reads-answer-for-the-credentials-tenant
    // Phase 1: coord's sender-side receipt read — the messaging family's
    // sender-side verb beside `coord_send_message` / `coord_inbox` / `coord_ack_message`,
    // so a sender can see whether its message was delivered and acked. coord's
    // device grant is the authority; this list only forwards.
    "coord_message_status",
    "coord_migration_queue",
    "coord_mute_gate",
    "coord_next_step_settings_effective",
    "coord_notify_sensitive_action",
    "coord_operator_touch_classify",
    "coord_orient",
    "coord_overlord_interventions",
    // Plan 2026-10-23-the-overlord-keeps-no-record-of-session-flow-so-its-development-runs-on-anecdote
    // Phase 2b: the triage-PASS ledger write (twin `POST /coord/overlord/passes`).
    // An append-only, self-attributed record of one /triage-sessions pass and its
    // per-terminal rows; it records what the overlord observed and did, and
    // performs nothing. Its two reads (`coord_query_overlord_passes`,
    // `coord_query_overlord_flow`) ride the `coord_query_` prefix below.
    "coord_overlord_pass_record",
    "coord_overlord_record",
    "coord_pending_agent_questions",
    "coord_pin_worktree",
    "coord_post_finding",
    // The agent-facing coord:* PR-label door (plan
    // 2026-08-27-coord-pr-label-write-path-single-door Phase 4a) — the pair an
    // ordinary session calls instead of the retired gh-then-POST skill shape.
    "coord_pr_label_set",
    "coord_pr_label_unset",
    "coord_pr_status",
    "coord_predict_resource_collisions",
    "coord_primary_tree_branch_status",
    // The one-screen project-state read (plan
    // 2026-09-20-what-is-the-state-of-my-projects-and-what-needs-me-is-answerable-from-one-screen,
    // Phase 3) — read-only; without it a runner-proxied session is refused it.
    "coord_project_state",
    // Plan 2026-09-20-trust-calibration-and-independent-verification-coverage-are-measured-continuously
    // Phase 4: the post-land re-verification lane's three coord tools. This one
    // is also covered by the `coord_query_` read-family prefix; it is named here
    // anyway so the lane's full tool set is visible in one place and survives a
    // future narrowing of that prefix. coord's `agent_tool_access::DEVICE_DEFAULT_TOOLS`
    // is the grant authority; this list only forwards.
    "coord_query_verification_metrics",
    "coord_recent_errors",
    "coord_recent_findings",
    "coord_record_decision",
    // Plan 2026-09-20-trust-calibration-… Phase 4: the verdict write door the
    // `/reverify-shipped` lane POSTs through (coord refuses a verifier that is
    // the unit's author; this list only forwards).
    "coord_record_verification",
    // Plan 2026-10-11-reverify-verdicts-are-source-bound-tree-checked-and-judge-agreement-is-measured
    // Phase 3: the ground-truth label write (twin `POST /coord/verification/labels`)
    // the judge-agreement measure is scored against. The label door's contract:
    // coord refuses an operator label from a non-operator principal, and an
    // agent records `seeded` labels only. Forwarded ahead of coord serving it
    // (the qontinui-coord label-door PR of the same plan); until that deploys,
    // coord answers it as an unknown tool. This list only forwards.
    "coord_record_verification_label",
    "coord_reevaluate",
    "coord_reevaluate_dry",
    "coord_register_gate",
    "coord_reject_gate",
    "coord_reopen_gate",
    // Plan 2026-10-17-per-repo-tenant-binding-overrides-the-org-tenant Phase 6:
    // the bindings read twin.
    "coord_repo_tenant_bindings",
    "coord_repoint_gate",
    "coord_report_status",
    "coord_request_handoff",
    "coord_resolve_origin",
    "coord_resolve_pr_author_session",
    "coord_resolve_session",
    "coord_secret_presence",
    "coord_send_message",
    "coord_session_obligations",
    "coord_session_worktrees",
    "coord_set_gate_audience",
    "coord_signature",
    "coord_slo_metrics",
    "coord_snooze_gate",
    "coord_submit_escalate_evidence",
    "coord_symbol_lookup",
    // Plan 2026-10-17-per-repo-tenant-binding-… Phase 6: the read twin of
    // GET /coord/repos/tenants (which tenant governs a repo), previously absent.
    "coord_tenants_for_repo",
    "coord_twin_catalog",
    "coord_typecheck_file",
    // Plan 2026-10-17-per-repo-tenant-binding-overrides-the-org-tenant Phase 6:
    // release / reclaim.
    "coord_unbind_repo_tenant",
    "coord_unmute_gate",
    // Plan 2026-09-20-trust-calibration-… Phase 4: the lane's queue read.
    "coord_verification_queue",
    "coord_watch_verdict",
    "coord_who_is_working_on",
    "coord_withdraw_agent_question",
    "coord_withdraw_gate",
    "coord_work_unit_add_citation",
    // Plan 2026-09-20-coord-delivery-has-no-verb-for-a-phase-that-ships-no-pr
    // Phase 4 (qontinui-coord#2616): attest a phase that ships no PR. It cannot
    // forge `shipped` — coord's I1+I2 mean an attestation never creates delivery
    // nor demotes; it only releases a phase-coverage pin on a unit that already
    // has a real landed PR, disclosed as a `PHASE ATTESTED WITHOUT A PR` gap.
    "coord_work_unit_attest_phase",
    "coord_work_unit_list",
    "coord_work_unit_list_citations",
    // Read-only; the project-state door's `row_count` is checked against it
    // (same plan, Phase 1 acceptance (d)).
    "coord_work_unit_overview",
    "coord_work_unit_refresh_citations",
    "coord_work_unit_remove_citation",
    // Same plan/phase: the undo of `coord_work_unit_attest_phase`. It writes the
    // attestation table, not citations, so it cannot erase a citation.
    "coord_work_unit_retract_phase_attestation",
    "coord_work_unit_transition",
    "coord_work_unit_upsert",
    "coord_write_prompt_document",
    "coord_yield",
];
const OLD_COORD_MCP_ALLOWED_TOOL_PREFIXES: &[&str] = &["coord_query_"];
const OLD_COORD_MCP_DELIBERATE_EXCLUSIONS: &[&str] = &[
    "coord_attest_advisory_ci_override",
    "coord_attest_escalate_override",
    "coord_cancel_merge",
    "coord_create_pr",
    "coord_enroll_repo",
    "coord_flag_state",
    "coord_flag_states",
    "coord_fleet_drain",
    "coord_fleet_drain_host",
    "coord_fleet_undrain",
    "coord_fleet_undrain_host",
    "coord_migration_reserve",
    "coord_pr_merge_profile",
    "coord_pr_merge_verdict",
    "coord_push_credential",
    "coord_push_to_branch",
    // Plan 2026-10-13-coord-expedited-land-lever-for-agents Phase 3b. Merge
    // authority, withheld on purpose: through this proxy a call carries the
    // runner's PAIRED-DEVICE JWT, which passes coord's possession check, and
    // coord's caller-session check is advisory — so a leaked session nonce
    // would reach a land lever. Merge tools are owed their own argument, not a
    // sweep. Sessions use the device-JWT HTTP twin instead
    // (`COORD_MCP_DELIBERATE_EXCLUSION_DOORS`), which the -32601 names.
    "coord_request_expedited_land",
    "coord_request_merge",
    "coord_request_policy",
    "coord_reserve_resource",
    "coord_set_pr_draft",
];
const OLD_COORD_MCP_DELIBERATE_EXCLUSION_DOORS: &[(&str, &str)] = &[(
    "coord_request_expedited_land",
    "POST /pr-merge/repos/:repo/pulls/:number/agent/expedite on coord with a device JWT and \
     your session id as X-Coord-Caller-Session (the expedited-land HTTP twin)",
)];
const OLD_COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES: &[&str] = &["coord_onboard"];

/// Every generated list equals its old const element for element, and
/// every element byte for byte (`&str` equality is byte equality).
#[test]
fn the_generated_lists_equal_the_old_consts() {
    assert_eq!(COORD_MCP_ALLOWED_METHODS, OLD_COORD_MCP_ALLOWED_METHODS);
    assert_eq!(COORD_MCP_ALLOWED_TOOLS, OLD_COORD_MCP_ALLOWED_TOOLS);
    assert_eq!(
        COORD_MCP_ALLOWED_TOOL_PREFIXES,
        OLD_COORD_MCP_ALLOWED_TOOL_PREFIXES
    );
    assert_eq!(
        COORD_MCP_DELIBERATE_EXCLUSIONS,
        OLD_COORD_MCP_DELIBERATE_EXCLUSIONS
    );
    assert_eq!(
        COORD_MCP_DELIBERATE_EXCLUSION_DOORS,
        OLD_COORD_MCP_DELIBERATE_EXCLUSION_DOORS
    );
    assert_eq!(
        COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES,
        OLD_COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES
    );
    // And as bytes, joined, so a difference shows as one readable diff.
    let bytes = |v: &[&str]| v.join("\n").into_bytes();
    assert_eq!(
        bytes(COORD_MCP_ALLOWED_TOOLS),
        bytes(OLD_COORD_MCP_ALLOWED_TOOLS)
    );
}

/// The served tool-policy lists render identically across the cutover:
/// the JSON this binary serves equals the JSON the OLD consts would have
/// produced, for every list and count key.
#[test]
fn the_served_tool_policy_lists_render_identically() {
    let served = coord_mcp_tool_policy_json();
    let old = serde_json::json!({
        "allowedTools": OLD_COORD_MCP_ALLOWED_TOOLS,
        "allowedToolPrefixes": OLD_COORD_MCP_ALLOWED_TOOL_PREFIXES,
        "allowedToolCount": OLD_COORD_MCP_ALLOWED_TOOLS.len(),
        "deliberateExclusions": OLD_COORD_MCP_DELIBERATE_EXCLUSIONS,
        "deliberateExclusionPrefixes": OLD_COORD_MCP_DELIBERATE_EXCLUSION_PREFIXES,
        "deliberateExclusionCount": OLD_COORD_MCP_DELIBERATE_EXCLUSIONS.len(),
    });
    for key in [
        "allowedTools",
        "allowedToolPrefixes",
        "allowedToolCount",
        "deliberateExclusions",
        "deliberateExclusionPrefixes",
        "deliberateExclusionCount",
    ] {
        assert_eq!(
            serde_json::to_string(&served[key]).expect("serializes"),
            serde_json::to_string(&old[key]).expect("serializes"),
            "{key} renders differently across the cutover"
        );
    }
}
