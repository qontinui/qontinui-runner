//! Source invariant: a module-local test serialiser is taken by EVERY test of
//! the module that defines it — or the module is on the allowlist with a
//! reason.
//!
//! `cargo test` runs a binary's tests as threads in one process. When a module
//! keeps process-global state and serialises "the tests that touch it" on a
//! module-local `static … Mutex<()>` — the `series_lock()` shape at
//! `mcp_api.rs` (`fn series_lock() -> MutexGuard<'static, ()>` over a fn-local
//! `static LOCK`), or a bare `static POOL_SERIAL: Mutex<()>` that tests
//! `.lock()` directly — the lock excludes only the tests that opt in. The next
//! test written in that module, by someone who did not read the doc comment
//! above the static, runs in parallel with all of them and reds one of them at
//! random with a panic that names an assertion, not the lock. Measured: the
//! Phase 0 census of plan `2026-09-17-runner-tests-share-in-process-mutable-state`
//! found `mcp_api::memory_search_enrichment_tests::a_skip_lands_in_its_own_series_and_not_in_enriched`
//! red 7/20 in the suite and green 6/6 alone — a module WITH a serialiser, and
//! a test that did not take it. A lock only opt-ins take "excludes nothing"
//! (the same lesson `2026-09-02-coord-unit-tests-share-process-globals-and-a-fixed-tmp-path`
//! recorded for qontinui-coord).
//!
//! This test is the source half of that plan's standing guard (Phase 5, D3):
//! the census (`scripts/test-interleave-census.mjs`, nightly in
//! `flake-escalation.yml`) is the OBSERVATION and finds the next singleton
//! whatever shape it takes; this guard is the one thing a source scan can
//! actually prove about the class — that no `#[cfg(test)]` module defines a
//! serialiser only some of its tests take. It ENUMERATES rather than trusting
//! the plan's prior-art table (which was already stale by eight sites when it
//! was written).
//!
//! # What counts
//!
//! A **test module** is a `mod` carrying a `#[cfg(…)]` attribute that names
//! `test` outside a `not(…)` (`#[cfg(test)]`, `#[cfg(any(test, debug_assertions))]`;
//! never `#[cfg(not(test))]`), any
//! ancestor of which does, or the root of a file whose path is a test file
//! (`tests.rs`, `*_tests.rs`, a `tests/` directory). A **test fn** is a fn
//! carrying an attribute whose path ends in `test`, as in
//! `env_write_lock_guard`.
//!
//! A **serialiser** is one of:
//!
//! * a module-level `static NAME: T` inside a test module whose type is a unit
//!   mutex — `Mutex<()>` under any qualifier or alias ending in `Mutex`
//!   (`std::sync::Mutex<()>`, `tokio::sync::Mutex<()>`, `StdMutex<()>`), bare
//!   or wrapped (`OnceLock<Mutex<()>>`, `Lazy<StdMutex<()>>`), together with
//!   every non-test fn of the same module that mentions it and returns a unit
//!   guard (`MutexGuard<'_, ()>`, `OwnedMutexGuard<()>`) — its **accessors**;
//! * a non-test fn inside a test module that returns a unit guard and declares
//!   such a static in its own body (the `series_lock()` shape — the static is
//!   reachable through the fn alone, so the fn IS the serialiser);
//! * a non-test fn inside a test module that returns a unit guard and declares
//!   no static at all — a **delegating accessor** (`health_lock()` in
//!   `device_jwt_refresher::tenant_slot_refresh_tests` returns
//!   `super::posture_test_lock()`). What it delegates to is someone else's
//!   population; what its own module's tests must do is call it — or take
//!   the lock-shaped callee (`…lock`) it delegates to directly, which is the
//!   same lock.
//!
//! A serialiser's **population** is every test fn in the module that defines
//! it, nested submodules included. A test **takes** a serialiser when its body
//! — closures, nested blocks and macro arguments included — names the static
//! or calls an accessor, or calls a fn defined in the same file that does
//! (closed transitively over same-file calls, keyed exactly as
//! `env_write_lock_guard` keys them: free fns by name, impl fns by
//! `Type::name`). A finding is a serialiser with at least one test in its
//! population that does not take it; the failure names the file, the module,
//! the serialiser and every missing test with its line.
//!
//! # Recognised and excluded by rule (printed, never silent)
//!
//! * A serialiser whose static or accessor is not private (`pub`,
//!   `pub(crate)`, `pub(super)`), or that is declared at file level under its
//!   own `#[cfg(test)]` rather than inside a test module — `posture_test_lock`
//!   (`device_jwt_refresher.rs`), `perf_test_lock` (`settings.rs`),
//!   `restore_forensics_lock` (`coord_mcp.rs`, `pub(super)`), `env_lock`
//!   (`ambient.rs`). Its population spans modules or files by design, so ONE
//!   file's source cannot enumerate it; the guard lists these as
//!   `cross-module` and asserts nothing about them. The module-local accessor
//!   that wraps one (`health_lock()`) is a delegating accessor and IS held to
//!   its own module.
//! * A unit-mutex static declared inside a non-test fn that returns no guard
//!   (`capture_logs_once` in `terminal/session.rs`): the helper holds the lock
//!   for its own duration, so every caller is serialised without opting in —
//!   not this class. The same rule leaves out a fn that returns an RAII
//!   handle WRAPPING the guard together with the state it protects
//!   (`pin_plan_capture_level_for_test` in `mcp/fleet_policy_poller.rs`,
//!   `MarkerOverride::set` in `coord_mcp.rs`): the state cannot be set without
//!   the lock, so there is nothing to forget — that is the per-test-handle
//!   shape the plan prefers, not the opt-in one. (A module-level static such
//!   a handle wraps is still enumerated on its own terms — `MARKER_OVERRIDE_LOCK`
//!   is listed as cross-module — and a test reaching the handle by a path call
//!   is credited with the static.)
//! * A unit-mutex static declared inside a `#[test]` fn's own body
//!   (`health_monitor.rs`, `observe_publishes_the_failure_count_before_it_reports`)
//!   serialises nothing — only that one test can reach it. Listed as
//!   `scoped-to-one-test` and reported as a finding of its own kind, because
//!   the doc comment above it says "serialise" and the lock does not.
//!
//! # Known limits
//!
//! * **Reach through a METHOD is not closed** — the same limit
//!   `env_write_lock_guard` documents, for the same reason: keying impl fns by
//!   bare name would credit any `X::new()` caller with an unrelated `Y::new()`'s
//!   lock. `MarkerOverride::set(..)` (a path call) IS reached; `x.set(..)` is not.
//! * **Taking is by NAME, not by liveness**: `let _ = series_lock();` drops the
//!   guard at once and still counts. The guard proves that a test reaches the
//!   serialiser, not that it holds it across the assertion.
//! * **A serialiser whose population is by design a subset** (the doc comment
//!   says "the two tests that mutate the store") is a finding here all the
//!   same — that IS the opt-in shape — and goes on the allowlist with that
//!   reason, where the reason is visible and the entry goes stale the day the
//!   module changes. The allowlist is a ratchet: an entry that no longer
//!   matches a finding fails this test by name.
//!
//! Registered in `main.rs` only, like `env_write_lock_guard`, whose `rs_files`
//! and `is_test_attr` it reuses. A sibling module rather than an extension of
//! that guard's `scan_source`: that visitor is flat (no module scoping, the
//! property it checks is file-wide), and this one's whole question is "which
//! module defines this and which tests are in it".

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

use crate::env_write_lock_guard::{is_test_attr, rs_files};

/// Floor for the walk, so a broken path or filter cannot pass vacuously
/// (`env_write_lock_guard` declares the same floor; 1559 files when written).
const MIN_FILES_WALKED: usize = 1000;
/// Floor for the enumerated population: the plan's prior-art table listed 23
/// `static … Mutex<()>` sites by grep, and this guard enumerated 21 test
/// serialisers when it was written (12 module-static, 3 fn-local, 2
/// delegating, 4 cross-module, 1 scoped-to-one-test; the grep's other hits
/// are production locks — `executor::restart_lock`, `coord_register`'s
/// `SPAWN`, `shim_materializer`'s `IDENTITY_MATERIALIZE_LOCK` — or a helper's
/// internal lock, `capture_logs_once`). Below this the detector broke, not
/// the tree. Printed by the guard, so re-measure with `--nocapture`.
const MIN_SERIALIZERS: usize = 15;

/// One allowlisted serialiser: the module, the lock, the tests outside it, why.
struct AllowlistEntry {
    file: &'static str,
    module: &'static str,
    serializer: &'static str,
    /// The EXACT test names the guard reports as not taking the serialiser.
    /// A `scoped-to-one-test` finding carries none.
    outside_the_lock: &'static [&'static str],
    reason: &'static str,
}

/// Sites the guard flags today that stay as they are: each with the reason, and
/// with the EXACT set of tests that stand outside the lock. Keyed by
/// `(file relative to src/, module path, serialiser label)`. Two ratchets, both
/// by name: an entry that matches no finding fails the guard (the module was
/// fixed, renamed or moved — remove the entry), and a finding whose missing set
/// is not EXACTLY `outside_the_lock` fails it too — a test added outside the
/// lock tomorrow is named, not silently accepted under the module's old reason,
/// which is the defect the module doc opens with. The sets were populated from
/// the guard's own enumeration (`cargo test opt_in_serializer_guard --
/// --nocapture` prints every finding), never from memory, so the counts the
/// reasons quote are checkable against them.
///
/// Every reason is one of two shapes. "scoped-to-one-test" is the one finding
/// whose kind is the finding. Every other reason states which tests stand
/// outside the lock and why that is safe TODAY — the resource the lock guards
/// is named, and the tests outside it do not reach that resource. That claim
/// is a source reading by the author of the entry, not a proof; the nightly
/// census is what checks it, and a `SUITE-ONLY` verdict on a module listed
/// here means the reason was wrong and the fix is to take the lock, not to
/// reword the entry.
const ALLOWLIST: &[AllowlistEntry] = &[
    AllowlistEntry {
        file: "mcp/device_jwt_refresher.rs",
        module: "tenant_slot_refresh_tests",
        serializer: "health_lock",
        outside_the_lock: &[
            "a_bound_tenant_with_no_slot_is_a_gap",
            "an_absent_sidecar_reports_unknown_never_no_gaps",
            "a_stale_sidecar_reports_unknown_never_no_gaps",
            "an_unreadable_covered_side_is_unknown_not_a_gap",
            "the_sweep_hop_composes_the_gap_report",
        ],
        reason: "the lock serialises refresh_tenant_slots passes over the process-global posture cell, \
         tenant_slot_health and the CLEARED_* counters; the five tests outside it \
         (a_bound_tenant_with_no_slot_is_a_gap, an_absent_sidecar_reports_unknown_never_no_gaps, \
         a_stale_sidecar_reports_unknown_never_no_gaps, an_unreadable_covered_side_is_unknown_not_a_gap, \
         the_sweep_hop_composes_the_gap_report) are pure over explicit inputs — \
         resolve_binding_gaps / binding_gaps_from on literal reads, and coord_bound_tenants_at on a \
         uniquely named temp dir — and never call refresh_tenant_slots or read the snapshot. \
         Enumerated after the 2026-09-21 rebase onto main, which added them; taking the lock in \
         them is the trivial alternative and lives in that file, not this one",
    },
    AllowlistEntry {
        file: "mcp/session_message_poller.rs",
        module: "tests",
        serializer: "COUNTER_TEST_LOCK",
        outside_the_lock: &[
            "delivered_message_is_not_redelivered",
            "session_in_cooldown_after_injection",
            "cooldown_expires_after_window",
            "prune_drops_expired_delivered_entries",
            "poll_interval_in_5_to_15s_window",
            "frame_message_carries_ack_instruction",
            "frame_message_defaults_blank_kind_and_priority",
            "frame_message_neutralizes_closer_in_body",
            "frame_message_neutralizes_closer_in_kind",
            "frame_message_neutralizes_closer_in_id_priority_and_from_session",
            "frame_message_neutralizes_uppercase_closer",
            "frame_message_neutralizes_whitespace_padded_closers",
            "frame_message_passes_ordinary_prose_through_byte_identical",
            "neutralizer_borrows_clean_text_and_owns_escaped_text",
            "surfacing_disabled_flag_never_fires",
            "surfacing_waits_for_threshold",
            "surfacing_repeat_is_spaced_by_repeat_not_threshold",
            "surfacing_cooldown_is_once_per_window",
            "tracker_fires_once_then_cools_down_then_fires_next_window",
            "tracker_reasons_are_tracked_independently",
            "a_busy_live_recipient_waits_the_repeat_window_before_the_first_post",
            "successful_delivery_clears_tracking",
            "retain_pending_drops_vanished_messages",
            "surfacing_env_parsing_defaults",
            "block_reason_wire_values",
            "open_record_resolves_terminal",
            "a_typed_record_routes_only_when_its_binding_is_known_and_confirmed",
            "closed_record_resolves_none",
            "absent_record_resolves_none",
            "blocked_log_fires_on_first_sighting_then_once_per_window",
            "blocked_log_is_independent_of_the_surfacing_flag",
            "delivery_loop_reports_every_priority",
            "delivered_arm_labels_match_health_keys",
            "typed_prompt_ready_admits_an_empty_or_placeholder_box",
            "typed_prompt_ready_refuses_a_half_typed_prompt_that_the_shared_predicate_admits",
            "typed_prompt_ready_refuses_a_selected_numbered_choice",
            "a_bare_shell_prompt_passes_the_screen_but_not_the_process_check",
            "stat_foreground_parse",
            "only_a_blocking_miss_opens_a_coord_alert",
            "gate_miss_details_name_the_typed_cases",
        ],
        reason: "the lock guards the process-global push counters (push_counters(), bumped only by \
         record_push_miss / record_push_ok, which production reaches through \
         surface_blocked_delivery and deliver_once); the only two tests that call either or \
         read health_snapshot() for a value are the two that take it — the other 40 exercise \
         the framing, tracker, cooldown and parse helpers and never touch a counter. Enumerated \
         after the 2026-09-21 rebase onto main, which added the module's counter family; the \
         per-test handle (MemoryEnrichCounters / TransportRungCounters in mcp_api.rs) is the \
         remedy that would delete this entry",
    },
    AllowlistEntry {
        file: "health_monitor.rs",
        module: "tests",
        serializer: "SERIAL",
        outside_the_lock: &[
        ],
        reason: "scoped-to-one-test: declared inside observe_publishes_the_failure_count_before_it_reports, \
         so it serialises that test against nothing; the other test that writes \
         BACKEND_WEDGED (stopping_the_monitor_clears_the_wedge_latches) asserts on the \
         latches it sets itself. Hoisting the static and taking it in both is a follow-up \
         recorded here, not silence",
    },
    AllowlistEntry {
        file: "agent_runtime.rs",
        module: "tests",
        serializer: "CONT_GUARD_LOCK",
        outside_the_lock: &[
            "condition_report_token_recovers_from_the_prompt",
            "condition_report_token_stops_at_quotes_and_whitespace",
            "condition_report_token_prefers_the_explicit_field",
            "condition_report_token_empty_field_falls_through",
            "condition_report_token_absent_is_none_not_a_guess",
            "condition_report_token_ignores_a_secret_in_the_auth_recipe",
            "condition_report_token_ignores_a_marker_in_a_condition_text",
            "condition_report_token_takes_the_last_shape_valid_occurrence",
            "condition_report_token_rejects_every_non_simple_uuid_shape",
            "condition_report_token_is_none_when_only_operator_text_matches",
            "condition_check_payload_debug_redacts_both_credential_carriers",
            "condition_run_report_body_is_an_explicit_error_status",
            "condition_check_payload_tolerates_a_coord_without_the_token_field",
            "headless_finalize_child_env_scrubs_credential_values",
            "headless_finalize_child_env_applies_non_interactive_git_posture",
            "headless_finalize_child_env_stamps_the_bound_port_not_the_configured_one",
            "agent_identity_env_override_wins",
            "agent_identity_falls_back_to_real_host_config",
            "agent_identity_rejects_x_placeholder_and_defaults",
            "agent_identity_rejects_empty_and_whitespace",
            "agent_identity_env_overrides_placeholder_host",
            "agent_identity_env_pairs_pin_author_and_committer",
            "pr_number_derived_only_from_pull_ref",
            "spawn_complete_body_enriched_shape",
            "spawn_failed_body_enriched_shape",
            "spawn_body_legacy_shape_when_unenriched",
            "pick_continuation_page_default_under_ceiling_picks_default",
            "pick_continuation_page_default_full_picks_nonfull_other",
            "pick_continuation_page_default_full_picks_fewest_terminals",
            "pick_continuation_page_tie_breaks_lexicographically",
            "pick_continuation_page_everything_full_mints",
            "pick_continuation_page_empty_counts_picks_default",
            "continuation_command_pins_session_id_before_positional_prompt",
            "continuation_command_add_dir_attached_form_keeps_prompt_positional",
            "continuation_command_single_repo_still_emits_terminator",
            "continuation_command_injects_system_prompt_before_terminator",
            "continuation_command_system_prompt_carries_source_marker",
            "continuation_command_injects_hook_settings_before_terminator",
            "continuation_command_omits_hook_settings_when_unavailable",
            "continuation_command_orders_the_full_injected_tail",
            "continuation_command_file_carrier_replaces_the_inline_flag_before_terminator",
            "a_template_replacement_prompt_withholds_the_seam_delivery",
            "continuation_command_never_emits_both_system_prompt_flags",
            "launch_payload_round_trips_through_envelope",
            "unknown_pinned_account_errors_instead_of_rotating",
            "logged_out_pinned_account_errors_instead_of_rotating",
            "absent_pin_leaves_rotation_untouched",
            "blank_pin_is_absence_not_a_bad_name",
            "resolved_pin_is_returned_for_the_child_env",
            "launch_payload_reads_optional_account_key",
            "launch_payload_accepts_legacy_plan_slug_key",
            "launch_payload_accepts_new_work_unit_slug_key",
            "launch_payload_accepts_coords_dual_emitted_both_keys",
            "launch_payload_new_key_wins_when_both_disagree",
            "launch_payload_absent_slug_is_none",
            "terminal_focus_request_serializes_with_terminal_id",
            "focus_existing_continuation_is_safe_headless",
            "coord_ws_url_resolves_on_hosted_tier_with_no_profile_coord_url",
            "coord_ws_url_is_none_when_isolated_or_tier_unknown",
            "coord_ws_url_agrees_with_the_gate_over_every_tier",
            "credential_door_200_launches_on_the_door_token_and_ignores_the_frame",
            "credential_door_200_with_missing_jti_is_nil_bookkeeping",
            "credential_door_200_with_empty_token_refuses",
            "credential_door_bare_404_with_frame_jwt_falls_back_to_the_frame",
            "credential_door_404_with_a_different_json_code_and_frame_jwt_falls_back",
            "credential_door_bare_404_with_empty_frame_jwt_refuses",
            "credential_door_typed_404_agent_not_found_never_falls_back",
            "credential_door_403_409_401_refuse_terminally_naming_status_and_code",
            "credential_door_refusal_without_json_body_names_none",
            "credential_door_503_defers_with_a_deferred_load_reason",
            "credential_door_unreachable_defers_even_with_a_frame_jwt",
            "credential_retry_policy_retries_transient_answers_then_stops",
            "credential_door_malformed_2xx_is_settled_and_refuses_naming_the_body",
            "credential_door_unconfigured_runner_is_settled_and_refuses",
            "credential_backoff_stop_reason_is_terminal_not_deferred",
            "credential_deferral_detail_is_bounded_and_sanitized",
            "launch_payload_without_jwt_fields_parses_with_empty_defaults",
            "coord_ws_string_payload_envelope_parses",
            "minimal_gate_continuation_does_not_parse_as_launch_payload",
            "gate_continuation_parses_with_and_without_presentation",
            "gate_continuation_payload_parses_coords_brief",
            "gate_continuation_payload_without_a_brief_parses_to_none",
            "gate_continuation_payload_survives_a_brief_shape_it_cannot_read",
            "a_brief_with_one_unreadable_metadata_field_still_carries_its_text",
            "an_unreadable_truncated_flag_never_claims_the_brief_is_complete",
            "the_brief_reaches_both_continuation_spawn_paths",
            "an_empty_brief_changes_neither_carrier",
            "a_truncated_brief_always_says_so",
            "the_brief_lands_in_append_system_prompt_argv",
            "source_routing_distinguishes_continuation_from_agent_spawn",
            "local_paths_strip_owner_slug",
            "provision_agent_defs_copies_md_files",
            "provision_agent_defs_missing_source_falls_back_to_the_embedded_floor",
            "stop_envelope_parses_agent_id",
            "request_agent_stop_missing_is_false",
            "agent_log_path_uses_agent_id",
            "claude_bin_respects_env_override",
            "fake_claude_e2e_smoke",
            "a_stalled_child_is_reported_but_never_touched",
            "terminal_continuation_command_is_interactive_positional_prompt",
            "a_qontinui_continuation_keeps_the_root_first_order_unverified",
            "a_foreign_repo_without_a_worktree_is_refused_not_placed",
            "a_repo_with_no_verified_checkout_is_refused_not_rooted",
            "a_held_worktree_claim_falls_through_without_waiting",
            "a_pty_trust_refusal_is_classified_blocked_and_everything_else_exited",
            "continuation_session_id_is_stable_for_same_anchor_and_device",
            "terminal_continuation_without_app_handle_fails_cleanly",
            "headless_continuation_passes_the_resolved_port_through",
            "headless_continuation_stays_fail_closed_on_an_unresolvable_port",
            "resolve_bound_api_port_is_none_without_a_tauri_runtime",
            "gate_continuation_headless_spawns_child",
            "a_connect_that_never_established_is_never_healthy_uptime",
            "backoff_resets_only_after_a_healthy_length_pump",
            "backoff_does_not_reset_on_subsecond_pump",
            "pending_continuations_response_parses_into_dispatchable_payloads",
            "pending_continuations_empty_response_parses",
            "claim_gate_dispatch_is_once_per_gate_id",
            "claim_dispatch_dispatch_is_once_per_dispatch_id",
            "gate_and_dispatch_dedupe_sets_are_independent",
            "release_gate_dispatch_allows_reclaim",
            "release_dispatch_dispatch_allows_reclaim",
            "release_local_dispatch_claim_routes_per_target",
            "superseded_skip_releases_local_dispatch_claim_so_relist_reclaims",
            "deferred_stamp_rate_limits_per_gate_per_hour",
            "continuation_poll_report_body_wire_shape",
            "pending_unit_dispatches_response_parses",
            "unit_dispatch_consumed_body_serializes_device_id_only",
            "launch_deferral_report_retry_is_bounded",
            "register_launch_stop_refuses_a_duplicate_and_keeps_the_original_token",
            "launch_stop_guard_released_on_panic_and_token_exact",
            "agent_run_teardown_over_poisoned_maps_drops_during_a_panic_unwind_and_removes_entries",
            "deferred_load_prefix_is_the_coord_wire_value",
            "gate_id_dedup_still_holds_alongside_anchor_guard",
            "continuation_addressed_to_self_matrix",
            "gate_continuation_parses_with_and_without_gate_id",
            "unit_dispatch_ws_frame_parses_with_dispatch_id_and_no_gate_id",
            "continuation_claim_body_wire_shape",
            "continuation_outcome_body_wire_shape",
            "runner_never_writes_work_completed",
            "outcome_ack_is_read_from_outcome_recorded_not_the_status",
            "session_work_outcomes_are_recognized_exactly",
            "reportable_gate_id_excludes_unit_dispatches",
            "first_line_takes_only_the_first_line",
            "decide_spawn_on_200_spawns",
            "decide_spawn_on_409_cancelled_skips_with_reason",
            "decide_spawn_on_409_superseded_skips_with_winner",
            "decide_spawn_on_409_superseded_without_winner_still_skips",
            "decide_spawn_on_409_cancelled_no_reason_still_skips",
            "decide_spawn_on_409_non_cancelled_proceeds",
            "decide_spawn_on_409_unparseable_proceeds",
            "decide_spawn_on_other_status_proceeds",
            "backstop_poll_secs_default_floor_and_override",
            "the_deferred_stamp_reasons_follow_the_class_detail_grammar",
            "compressed_jwt_exp_honors_env_override",
            "headless_presentation_never_waits_for_tauri",
            "terminal_presentation_is_ready_once_setup_marked_it",
            "terminal_presentation_defers_while_booting_and_is_absent_past_the_grace",
            "presentation_boot_grace_outlasts_the_backstop_tick",
            "allocated_worktree_without_freshness_fields_reads_unknown",
            "allocated_worktree_round_trips_a_stale_verdict",
            "allocated_worktree_explicit_nulls_read_unmeasured",
            "allocated_worktree_unrecognised_freshness_reads_unknown",
            "payload_to_allocate_result_carries_freshness_onto_the_result",
        ],
        reason: "guards the continuation registry + admitted-launch cap (clear_continuation_registry); \
         the 163 tests outside it never call evaluate_continuation_guard* or the registry \
         accessors (grep-verified 2026-09-21) — command builders, payload shapes, env scrubs",
    },
    AllowlistEntry {
        file: "ai_provider/oauth_refresh.rs",
        module: "tests",
        serializer: "REFRESH_LOG_GUARD",
        outside_the_lock: &[
            "has_valid_credentials_false_when_dir_has_no_creds_file",
            "has_valid_credentials_true_when_unexpired",
            "only_grant_rejections_count_as_hard_failures",
            "hard_failures_back_off_exponentially_up_to_a_cap",
            "has_valid_credentials_checks_the_exact_dir_not_a_fallback",
        ],
        reason: "guards REFRESH_REQUESTS (the recorded background-refresh log); the 5 tests outside it \
         (has_valid_credentials_*, hard-failure backoff) neither queue nor drain a request",
    },
    AllowlistEntry {
        file: "capability_manifest.rs",
        module: "tests",
        serializer: "store_lock",
        outside_the_lock: &[
            "rung_all_covers_every_variant",
            "rung_wire_strings_are_stable_literals",
            "rung_wire_strings_are_distinct",
            "rung_descriptions_and_column_width_hold",
            "rung_serializes_as_its_wire_string",
            "rung_predicates_partition_the_vocabulary",
            "workspace_root_kind_maps_into_every_rung_it_should",
            "workspace_root_observation_preserves_the_upstream_kind_and_rejection",
            "every_command_source_maps_to_a_distinct_rung_with_no_caveat",
            "command_source_observation_states_the_upstream_variant",
            "skill_source_maps_the_typed_values_and_never_guesses",
            "a_degraded_pass_reports_its_skipped_units_with_reasons",
            "a_complete_pass_carries_no_skip_note",
            "an_unresolved_pass_states_the_rung_and_the_reason",
            "skip_reasons_serialize_as_a_tagged_shape",
            "capability_specs_are_the_seeded_roster_with_unique_ids",
            "every_capability_spec_is_fully_populated",
            "every_capability_has_an_input_field",
            "unobserved_rows_render_unknown_naming_their_owning_symbol_and_are_never_omitted",
            "an_injected_observation_reaches_the_row_intact",
            "unresolved_count_covers_both_non_answers_and_the_render_separates_them",
            "manifest_carries_build_identity_and_schema_version",
            "for_this_build_reports_this_binary_and_observes_nothing",
            "manifest_doc_mentions_every_capability_and_every_rung",
            "manifest_doc_is_byte_stable_and_carries_no_runtime_value",
            "text_render_lists_every_row_and_prints_rejections_on_success",
            "rung_rank_is_the_declared_ordering_with_the_non_answers_last",
            "the_roster_matches_the_resolvers_phase_2_actually_shipped",
            "manifest_json_is_wellformed_and_uses_wire_strings",
        ],
        reason: "guards the process-wide provisioning store (reset_provision_store); the 29 tests \
         outside it never touch the store — spec population, unit shapes, rendering",
    },
    AllowlistEntry {
        file: "commands/transcript.rs",
        module: "tests",
        serializer: "CACHE_TESTS_ARE_SERIAL",
        outside_the_lock: &[
            "fresh_entry_with_matching_inputs_is_servable",
            "expired_entry_is_not_servable",
            "differing_inputs_are_not_servable",
            "the_scan_path_uses_no_tokio_timer",
        ],
        reason: "guards the scan cache, its counters and the process-wide scan dispatcher; the 4 tests \
         outside it are pure (entry_is_servable over arguments) or read this file's source",
    },
    AllowlistEntry {
        file: "embedded_pg.rs",
        module: "tests",
        serializer: "PG_TEST_LOCK",
        outside_the_lock: &[
            "data_root_override_wins_over_the_shared_default",
            "unset_data_root_falls_back_to_the_shared_default",
            "blank_data_root_override_is_treated_as_unset",
            "override_is_trimmed_before_use",
            "default_data_root_is_the_historical_shared_path",
            "ready_pid_file_yields_its_port",
            "starting_status_does_not_attach",
            "non_ready_statuses_do_not_attach",
            "truncated_pid_file_does_not_attach",
            "non_numeric_port_does_not_attach",
            "garbage_pid_line_does_not_attach",
            "ready_pid_file_with_refused_port_does_not_attach",
            "missing_pid_file_does_not_attach",
            "ready_pid_file_with_live_port_attaches",
            "db_arm_wire_names_round_trip",
            "unknown_arm_code_reads_as_unknown",
            "leaving_an_embedded_arm_clears_the_port",
            "attached_handle_reports_its_joined_port",
        ],
        reason: "a tokio::sync::Mutex serialising the tests that boot a PostgreSQL cluster (disk, port \
         and archive contention); the 18 tests outside it parse pid files and probe a per-test \
         tempdir and never start a server",
    },
    AllowlistEntry {
        file: "env_agent/enroll.rs",
        module: "tests",
        serializer: "slot_lock",
        outside_the_lock: &[
            "enroll_carries_a_declared_scope_root_forward",
            "enroll_leaves_scope_root_unset_when_there_was_none",
            "enroll_carries_the_repo_owner_allowlist_forward",
            "enroll_leaves_the_allowlist_empty_when_there_was_none",
            "resolve_backend_prefers_explicit_and_trims_slash",
            "enroll_request_serializes_coord_device_id_only_when_present",
            "present_machine_json_yields_no_devenv_machine_id_but_keeps_coord_device_id",
            "legacy_machine_id_key_parses_as_coord_device_id",
            "non_uuid_device_id_is_still_present",
            "unparseable_machine_json_yields_absent_identity",
        ],
        reason: "guards ENROLL_IN_FLIGHT; the 10 tests outside it never reach with_enroll_slot or \
         run_enroll — request serialisation, backend resolution, machine.json parsing",
    },
    AllowlistEntry {
        file: "mcp/test_fixtures.rs",
        module: "tests",
        serializer: "TEST_LOCK",
        outside_the_lock: &[
            "module_cfg_gate_is_first_non_comment_line",
            "mod_declaration_is_cfg_gated",
            "mcp_api_routes_merge_is_cfg_gated",
            "all_test_routes_remain_wired",
            "the_counts_vocabulary_produces_the_named_buckets",
            "routes_construct_without_panic",
            "project_short_circuit_statuses_carry_override_no_tab",
            "project_tab_backed_idle_emits_live_pre_aged_tab",
            "project_tab_backed_idle_clamps_quiet_to_floor",
            "project_tab_backed_error_and_completed_emit_dead_tab",
            "seed_lifecycle_store_round_trip_list_open_and_clear",
            "seed_lifecycle_store_rejects_malformed_body",
            "seed_agent_token_registers_slot_and_agent_bound_nonce",
            "seed_agent_token_rejects_empty_fields",
            "agent_token_view_present_and_absent",
            "seed_lifecycle_store_drops_a_stale_wal_that_would_replay_over_it",
            "a_running_store_adopts_the_seed_instead_of_overwriting_it",
            "reload_from_disk_replaces_rather_than_merges",
            "clear_lifecycle_store_empties_the_running_store_not_just_the_file",
            "clear_then_reseed_reads_back_exactly_the_reseeded_rows",
            "clear_lifecycle_store_drops_the_wal_that_would_replay_the_cleared_rows",
            "a_file_read_back_lies_about_a_live_store_the_in_store_read_does_not",
            "an_empty_seed_body_is_rejected_and_names_the_clear_route",
            "a_seed_can_now_express_confirmation_pending_tier_and_origin",
            "a_seed_that_omits_them_still_produces_an_unconfirmed_row",
            "a_confirmed_seeded_row_projects_as_restorable",
            "seeded_pending_and_failed_rows_reach_their_buckets",
            "append_transcript_record_lands_at_the_path_the_reader_opens",
            "append_transcript_record_is_returned_by_the_real_reader",
            "append_moves_the_mtime_so_the_next_read_is_not_short_circuited",
            "back_to_back_appends_each_move_the_mtime",
            "every_record_kind_reaches_the_readers_expected_verdict",
            "reset_truncates_before_appending_and_the_default_appends",
            "an_append_onto_a_newline_less_transcript_stays_valid_jsonl",
            "a_caller_supplied_uuid_and_timestamp_are_used_verbatim",
            "append_rejects_an_empty_triple_or_an_escaping_session_id",
            "the_written_flags_are_the_ones_the_reader_filters_on",
            "record_kinds_serialize_with_their_documented_wire_names",
            "a_seed_can_now_bind_a_live_terminal_id_and_config_dir",
            "a_seed_that_omits_the_binding_fields_keeps_the_historical_row",
            "a_seeded_row_binds_to_a_live_terminal_id_through_the_terminal_list_lookup",
            "the_read_back_door_reports_the_seeded_binding",
            "the_read_back_door_shows_an_unbound_row_as_the_synthetic_id",
            "the_open_id_list_is_exactly_the_record_lists_session_ids",
            "the_read_back_door_serializes_the_binding_fields_under_stable_keys",
            "a_blank_terminal_id_is_rejected_rather_than_binding_to_nothing",
            "a_blank_config_dir_is_rejected_rather_than_seeding_an_empty_path",
            "a_blank_binding_field_fails_the_whole_seed_with_a_400",
        ],
        reason: "guards the registry() singleton; the 48 tests outside it project statuses and \
         parse route tables and never call registry()",
    },
    AllowlistEntry {
        file: "process_helpers.rs",
        module: "timeout_tests",
        serializer: "GAUGE_TESTS",
        outside_the_lock: &[
            "a_blocking_child_returns_within_the_budget_and_is_reaped",
            "run_probe_degrades_within_budget_and_reaps",
            "run_probe_distinguishes_a_failing_child_from_a_hung_one",
            "run_probe_quiet_degrades_the_same_way_on_a_negative_answer",
            "output_with_timeout_reports_a_hang_as_an_io_timeout",
            "output_with_timeout_never_leaks_argv_into_its_error",
            "a_caller_supplied_label_is_used_verbatim",
            "hung_probes_do_not_permanently_consume_the_blocking_pool",
            "a_flood_of_stdout_is_capped_and_reported_as_truncated",
            "an_interrupted_wait_reports_timed_out_instead_of_ready",
            "a_fast_child_completes_normally_with_its_stdout",
            "a_chatty_child_has_all_of_its_output_captured",
        ],
        reason: "guards assertions on the live_pipe_readers() gauge (two tests, one of which leaves \
         readers alive on purpose); the 12 tests outside it spawn children that bump the \
         gauge but assert nothing about it, as the static's own doc says",
    },
    AllowlistEntry {
        file: "terminal/auto_response.rs",
        module: "tests",
        serializer: "RULES_TEST_LOCK",
        outside_the_lock: &[
            "resolve_response_injects_only_on_resolved_true",
            "resolve_response_parses_both_coord_shapes",
            "scoring_prompt_includes_options_dimensions_context_and_demands_json",
            "scoring_prompt_with_no_dimensions_asks_for_overall",
            "parse_scores_clean_json",
            "parse_scores_code_fenced_json",
            "parse_scores_garbage_is_none",
            "resolve_fires_submits_when_scorer_and_coord_agree",
            "resolve_no_op_when_scorer_returns_none",
            "resolve_no_op_when_coord_unresolved",
            "resolve_no_op_when_no_options",
            "grid_scan_is_edge_triggered_fire_once_per_appearance",
            "grid_scan_multiple_rules_independent_edges",
            "compute_delay_unbounded",
            "compute_delay_capped",
            "compute_delay_saturates_without_panic",
            "register_match_no_stack_while_pending",
            "reset_window_restarts_attempts",
        ],
        reason: "guards COMPILED_RULES (reload_rules / rules_active / process); the 18 tests outside \
         it score prompts, parse JSON and compute delays over their own values",
    },
    AllowlistEntry {
        file: "terminal/mod.rs",
        module: "tests",
        serializer: "quiet_credential_posture",
        outside_the_lock: &[
            "credential_env_list_covers_the_three_known_plaintext_passwords",
            "credential_env_list_excludes_identifier_variables",
            "credential_env_list_entries_are_credential_values",
            "scrub_removes_inherited_values_from_a_pty_command",
            "scrub_records_removals_on_a_tokio_command",
            "scrub_records_removals_on_a_std_command",
            "source_marker_carries_build_identity",
            "plan_capture_fallback_clause_names_both_doors_and_the_playbook",
            "memory_clause_starts_with_a_blank_line_so_it_never_touches_line_one",
            "memory_clause_names_both_tools_and_the_search_argument",
            "memory_clause_has_no_collapsed_whitespace_runs",
            "memory_clause_carries_no_tenant_identity",
            "memory_clause_conditional_carries_no_tenant_identity",
            "memory_clause_conditional_starts_with_a_blank_line_so_it_never_touches_line_one",
            "memory_clause_conditional_names_both_tools_and_the_search_argument",
            "memory_clause_conditional_has_no_collapsed_whitespace_runs",
            "the_api_port_reaches_the_rendered_briefing",
            "spawn_seam_api_port_prefers_the_bound_port_over_the_configured_one",
            "spawn_seam_api_port_falls_back_when_no_bind_has_been_recorded",
            "strip_ansi_removes_bracketed_paste_markers_and_keeps_the_body",
            "strip_ansi_keeps_text_around_a_bracketed_paste_block",
            "strip_ansi_removes_an_st_terminated_osc_and_keeps_the_rest",
            "strip_ansi_removes_an_st_terminated_osc_52",
            "strip_ansi_consumes_dcs_sos_pm_and_apc_payloads",
            "strip_ansi_unterminated_csi_keeps_the_remainder",
            "strip_ansi_unterminated_osc_keeps_the_remainder",
            "strip_ansi_round_trips_unicode_outside_sequences",
            "strip_ansi_removes_a_sequence_with_a_non_ascii_payload",
        ],
        reason: "delegates to the crate-wide posture_test_lock(); the tests outside it that render \
         runner_context() assert on lines the posture does not write (source marker, api \
         port, memory clause) or take posture_test_lock() directly and are credited",
    },
    AllowlistEntry {
        file: "wedge_diagnostics.rs",
        module: "tests",
        serializer: "POOL_SERIAL",
        outside_the_lock: &[
            "no_call_site_discards_a_blocking_slot",
            "the_public_spawn_wrapper_only_delegates",
            "lane_resolution_routes_to_the_table_it_was_handed",
            "a_saturated_lane_table_degrades_to_the_overflow_bucket",
            "the_per_runtime_capacity_is_tokios_documented_default",
            "the_record_is_one_json_object_per_line",
            "the_wire_format_is_pinned",
            "the_record_cannot_be_read_as_single_pool_occupancy",
            "an_unavailable_field_serializes_as_its_reason_string",
            "a_huge_child_population_stays_a_bounded_line",
            "cap_tally_keeps_the_most_frequent_deterministically",
            "retention_keeps_the_newest_fourteen_and_nothing_else",
            "retention_is_a_no_op_below_the_window",
            "only_our_own_rolled_files_are_recognised",
            "retention_actually_unlinks_on_disk",
            "a_hanging_capture_step_times_out_within_budget",
            "a_timed_out_capture_still_produces_a_complete_record",
            "a_healthy_capture_lands_a_value_record_in_the_days_file",
            "a_round_trip_returns_the_steps_own_output",
            "the_thread_census_sees_this_test_process",
            "the_child_census_runs_and_counts_a_real_child",
            "a_spinning_child_reports_meaningful_cpu",
            "proc_stat_is_parsed_from_the_last_paren",
            "wait_reasons_render_the_kernels_names",
            "linux_states_render_long_hand",
        ],
        reason: "guards the process-global LANES slot counts; the 25 tests outside it drive a private \
         LaneTable (fresh_lane_table / spawn_blocking_tracked_in), scan this file's source, \
         or measure a child process — the per-test-handle shape this plan generalises",
    },
];

/// How a serialiser was recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A module-level unit-mutex static (with zero or more accessors).
    ModuleStatic,
    /// A non-test fn returning a unit guard over a static declared in its body.
    FnLocal,
    /// A non-test fn returning a unit guard that declares no static of its own.
    Delegating,
    /// A unit-mutex static declared inside a `#[test]` fn — serialises nothing.
    ScopedToOneTest,
    /// Not private, or declared at file level under `#[cfg(test)]`: its
    /// population is not one module's. Enumerated, asserted about nothing.
    CrossModule,
}

/// One serialiser the scanner recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Serializer {
    /// Module path within the file (`tests`, `tests::nested`), `""` at file level.
    module: String,
    /// The static's name for `ModuleStatic` / `ScopedToOneTest` / `CrossModule`
    /// statics; the fn's name for `FnLocal` / `Delegating`.
    label: String,
    line: usize,
    kind: Kind,
    /// Every name a test may take it by: the static (when module-level) and
    /// each accessor fn.
    handles: BTreeSet<String>,
    /// Why it is `CrossModule`, when it is.
    why: Option<String>,
}

/// A test fn in a serialiser's population that does not take it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MissingTest {
    line: usize,
    name: String,
}

/// One serialiser with its population verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    serializer: Serializer,
    population: usize,
    missing: Vec<MissingTest>,
}

/// The verdict for one source file.
#[derive(Debug, Default)]
struct FileReport {
    /// Every serialiser recognised, of every kind.
    serializers: Vec<Serializer>,
    /// Serialisers with a non-empty population and at least one missing test,
    /// plus every `ScopedToOneTest` (whose finding is its kind).
    findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------
// Type shapes
// ---------------------------------------------------------------------------

fn is_unit_tuple(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(t) if t.elems.is_empty())
}

fn type_args(seg: &syn::PathSegment) -> Vec<&syn::Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a
            .args
            .iter()
            .filter_map(|g| match g {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `…Mutex<()>`, bare or wrapped in any generic (`OnceLock<Mutex<()>>`,
/// `Lazy<StdMutex<()>>`).
fn is_unit_mutex_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(last) = tp.path.segments.last() else {
        return false;
    };
    let args = type_args(last);
    if last.ident.to_string().ends_with("Mutex") && args.len() == 1 && is_unit_tuple(args[0]) {
        return true;
    }
    tp.path
        .segments
        .iter()
        .flat_map(type_args)
        .any(is_unit_mutex_type)
}

/// `…MutexGuard<'_, ()>` / `…OwnedMutexGuard<()>`: a guard over a unit mutex.
fn is_unit_guard_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(last) = tp.path.segments.last() else {
        return false;
    };
    last.ident.to_string().ends_with("MutexGuard")
        && type_args(last).iter().any(|t| is_unit_tuple(t))
}

fn returns_unit_guard(sig: &syn::Signature) -> bool {
    match &sig.output {
        syn::ReturnType::Type(_, ty) => is_unit_guard_type(ty),
        syn::ReturnType::Default => false,
    }
}

/// `#[cfg(test)]`, `#[cfg(any(test, …))]` — a `cfg` attribute naming `test`
/// OUTSIDE a `not(…)`. `#[cfg(not(test))]` is the production arm and names
/// `test` only to exclude it; `#[cfg(all(not(test), …))]` likewise. A `test`
/// under `not(` never counts; one beside it does (`any(test, not(debug))`).
fn is_cfg_test_attr(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    fn names_test(tokens: TokenStream) -> bool {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        trees.iter().enumerate().any(|(i, t)| match t {
            TokenTree::Ident(id) => id == "test",
            TokenTree::Group(g) => {
                let negated = i >= 1 && matches!(&trees[i - 1], TokenTree::Ident(p) if p == "not");
                !negated && names_test(g.stream())
            }
            _ => false,
        })
    }
    attr.parse_args::<TokenStream>().is_ok_and(names_test)
}

fn is_private(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Inherited)
}

fn vis_label(vis: &syn::Visibility) -> String {
    match vis {
        syn::Visibility::Public(_) => "pub".to_string(),
        syn::Visibility::Restricted(r) => {
            let p: Vec<String> = r
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            format!("pub({})", p.join("::"))
        }
        syn::Visibility::Inherited => "private".to_string(),
    }
}

/// Is `path` a test file by name: `tests.rs`, `*_tests.rs`, or under `tests/`.
fn is_test_file(rel: &str) -> bool {
    let stem = Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    stem == "tests"
        || stem.ends_with("_tests")
        || rel.contains("/tests/")
        || rel.starts_with("tests/")
}

// ---------------------------------------------------------------------------
// Body facts
// ---------------------------------------------------------------------------

/// What one fn body does DIRECTLY — before same-file calls are resolved.
#[derive(Debug, Default)]
struct BodyFacts {
    /// Every identifier the body names through a path or a macro's tokens.
    idents: BTreeSet<String>,
    /// Every path call in the body, as `name` and `Qualifier::name` (with
    /// `Self` resolved to the enclosing impl's type) — the same keying as
    /// `env_write_lock_guard`.
    calls: BTreeSet<String>,
    /// Unit-mutex statics declared inside the body.
    local_statics: Vec<(String, usize)>,
}

fn scan_tokens(tokens: TokenStream, facts: &mut BodyFacts, self_ty: Option<&str>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(g) => scan_tokens(g.stream(), facts, self_ty),
            TokenTree::Ident(id) => {
                let name = id.to_string();
                facts.idents.insert(name.clone());
                let is_call = matches!(
                    trees.get(i + 1),
                    Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Parenthesis
                );
                let is_method =
                    i >= 1 && matches!(&trees[i - 1], TokenTree::Punct(p) if p.as_char() == '.');
                if !is_call || is_method {
                    continue;
                }
                let qualifier = match i
                    .checked_sub(3)
                    .map(|q| (&trees[q], &trees[q + 1], &trees[q + 2]))
                {
                    Some((TokenTree::Ident(q), TokenTree::Punct(a), TokenTree::Punct(b)))
                        if a.as_char() == ':' && b.as_char() == ':' =>
                    {
                        Some(q.to_string())
                    }
                    _ => None,
                };
                facts.calls.insert(name.clone());
                if let Some(q) = qualifier {
                    let q = if q == "Self" {
                        self_ty.unwrap_or(&q).to_string()
                    } else {
                        q
                    };
                    facts.calls.insert(format!("{q}::{name}"));
                }
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

struct BodyScanner {
    facts: BodyFacts,
    self_ty: Option<String>,
}

impl<'ast> Visit<'ast> for BodyScanner {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            if let Some(name) = segs.last() {
                self.facts.calls.insert(name.clone());
                if segs.len() >= 2 {
                    let q = &segs[segs.len() - 2];
                    let q = if q == "Self" {
                        self.self_ty.clone().unwrap_or_else(|| q.clone())
                    } else {
                        q.clone()
                    };
                    self.facts.calls.insert(format!("{q}::{name}"));
                }
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        for s in &p.segments {
            self.facts.idents.insert(s.ident.to_string());
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        scan_tokens(m.tokens.clone(), &mut self.facts, self.self_ty.as_deref());
        visit::visit_macro(self, m);
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if is_unit_mutex_type(&s.ty) {
            self.facts
                .local_statics
                .push((s.ident.to_string(), s.ident.span().start().line));
        }
        visit::visit_item_static(self, s);
    }
}

// ---------------------------------------------------------------------------
// Module-aware collection
// ---------------------------------------------------------------------------

struct FnRecord {
    /// `name` for a free fn, `Type::name` for an impl/trait method.
    key: String,
    name: String,
    line: usize,
    module: String,
    in_test_module: bool,
    is_test: bool,
    /// The item itself carries `#[cfg(test)]` (a file-level test-only fn).
    cfg_test_item: bool,
    private: bool,
    vis: String,
    returns_unit_guard: bool,
    facts: BodyFacts,
}

struct StaticRecord {
    name: String,
    line: usize,
    module: String,
    in_test_module: bool,
    /// The item itself carries `#[cfg(test)]` (a file-level test-only static).
    cfg_test_item: bool,
    private: bool,
    vis: String,
}

struct Collector {
    /// Module path segments, outermost first.
    modules: Vec<String>,
    /// Whether each enclosing module (and the file root) is test-gated.
    test_flags: Vec<bool>,
    /// The enclosing impl's self type, innermost last; `None` inside a fn body.
    scope: Vec<Option<String>>,
    fns: Vec<FnRecord>,
    statics: Vec<StaticRecord>,
}

impl Collector {
    fn new(file_root_is_test: bool) -> Self {
        Self {
            modules: Vec::new(),
            test_flags: vec![file_root_is_test],
            scope: Vec::new(),
            fns: Vec::new(),
            statics: Vec::new(),
        }
    }

    fn module_path(&self) -> String {
        self.modules.join("::")
    }

    fn in_test_module(&self) -> bool {
        self.test_flags.iter().any(|f| *f)
    }

    fn record_fn(
        &mut self,
        ident: &syn::Ident,
        attrs: &[syn::Attribute],
        vis: &syn::Visibility,
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
        let self_ty = self.scope.last().cloned().flatten();
        let mut scanner = BodyScanner {
            facts: BodyFacts::default(),
            self_ty: self_ty.clone(),
        };
        scanner.visit_block(block);
        let name = ident.to_string();
        self.fns.push(FnRecord {
            key: match &self_ty {
                Some(t) => format!("{t}::{name}"),
                None => name.clone(),
            },
            line: ident.span().start().line,
            module: self.module_path(),
            in_test_module: self.in_test_module(),
            is_test: attrs.iter().any(is_test_attr),
            cfg_test_item: attrs.iter().any(is_cfg_test_attr),
            private: is_private(vis),
            vis: vis_label(vis),
            returns_unit_guard: returns_unit_guard(sig),
            name,
            facts: scanner.facts,
        });
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        self.modules.push(m.ident.to_string());
        self.test_flags.push(m.attrs.iter().any(is_cfg_test_attr));
        visit::visit_item_mod(self, m);
        self.test_flags.pop();
        self.modules.pop();
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        // Only ITEM-level statics land here: a fn body's statics are seen by
        // the fn's own `BodyScanner`, and `visit_item_fn` below does not
        // descend into the block through this visitor.
        if is_unit_mutex_type(&s.ty) {
            self.statics.push(StaticRecord {
                name: s.ident.to_string(),
                line: s.ident.span().start().line,
                module: self.module_path(),
                in_test_module: self.in_test_module(),
                cfg_test_item: s.attrs.iter().any(is_cfg_test_attr),
                private: is_private(&s.vis),
                vis: vis_label(&s.vis),
            });
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.record_fn(&f.sig.ident, &f.attrs, &f.vis, &f.sig, &f.block);
        // Nested fn items inside the body are recorded as free fns of this
        // module (their statics belong to the enclosing fn's facts already).
        self.scope.push(None);
        for stmt in &f.block.stmts {
            if let syn::Stmt::Item(item) = stmt {
                if !matches!(item, syn::Item::Static(_)) {
                    self.visit_item(item);
                }
            }
        }
        self.scope.pop();
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.scope.push(ty);
        visit::visit_item_impl(self, i);
        self.scope.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.record_fn(&f.sig.ident, &f.attrs, &f.vis, &f.sig, &f.block);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if let Some(block) = &f.default {
            self.record_fn(
                &f.sig.ident,
                &f.attrs,
                &syn::Visibility::Inherited,
                &f.sig,
                block,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

fn in_module(candidate: &str, module: &str) -> bool {
    module.is_empty() || candidate == module || candidate.starts_with(&format!("{module}::"))
}

/// Does any call in `facts` reach a fn (other than `me`) whose flag is set?
fn reaches(
    facts: &BodyFacts,
    me: usize,
    by_key: &HashMap<&str, Vec<usize>>,
    flags: &[bool],
) -> bool {
    facts.calls.iter().any(|k| {
        by_key
            .get(k.as_str())
            .is_some_and(|ix| ix.iter().any(|&j| j != me && flags[j]))
    })
}

/// Check one file's source. `Err` only when it does not parse.
fn scan_source(src: &str, rel: &str) -> syn::Result<FileReport> {
    let file = syn::parse_file(src)?;
    let mut c = Collector::new(is_test_file(rel));
    c.visit_file(&file);
    let Collector { fns, statics, .. } = c;

    let mut serializers: Vec<Serializer> = Vec::new();

    // Module-level statics.
    for s in &statics {
        let accessors: BTreeSet<String> = fns
            .iter()
            .filter(|f| {
                !f.is_test
                    && f.returns_unit_guard
                    && f.module == s.module
                    && f.facts.idents.contains(&s.name)
            })
            .map(|f| f.name.clone())
            .collect();
        let non_private_accessor = fns.iter().find(|f| {
            !f.is_test
                && f.returns_unit_guard
                && f.module == s.module
                && accessors.contains(&f.name)
                && !f.private
        });
        let mut handles = accessors.clone();
        handles.insert(s.name.clone());
        let (kind, why) = if !s.in_test_module {
            if s.cfg_test_item {
                (
                    Kind::CrossModule,
                    Some("declared at file level under its own #[cfg(test)]".to_string()),
                )
            } else {
                // A production static (`IDENTITY_MATERIALIZE_LOCK`): not a test
                // serialiser at all.
                continue;
            }
        } else if !s.private {
            (Kind::CrossModule, Some(format!("static is {}", s.vis)))
        } else if let Some(a) = non_private_accessor {
            (
                Kind::CrossModule,
                Some(format!("accessor {}() is {}", a.name, a.vis)),
            )
        } else {
            (Kind::ModuleStatic, None)
        };
        serializers.push(Serializer {
            module: s.module.clone(),
            label: s.name.clone(),
            line: s.line,
            kind,
            handles,
            why,
        });
    }

    // Fn-local statics and delegating accessors: non-test fns in a test
    // module that return a unit guard.
    for f in &fns {
        if f.is_test || !f.returns_unit_guard {
            continue;
        }
        let over_module_static = statics
            .iter()
            .any(|s| s.module == f.module && f.facts.idents.contains(&s.name));
        if over_module_static {
            continue; // already an accessor of that static
        }
        let has_local = !f.facts.local_statics.is_empty();
        let (kind, why) = if !f.in_test_module {
            if has_local && f.cfg_test_item {
                // `posture_test_lock` / `perf_test_lock`: a file-level fn over its
                // own static, cfg(test)-gated by attribute rather than module.
                (
                    Kind::CrossModule,
                    Some(format!("declared at file level, {}", f.vis)),
                )
            } else {
                // Production code, or a test-only delegate outside any module:
                // not a test serialiser this guard can place.
                continue;
            }
        } else if !f.private {
            (Kind::CrossModule, Some(format!("accessor is {}", f.vis)))
        } else if has_local {
            (Kind::FnLocal, None)
        } else {
            (Kind::Delegating, None)
        };
        let mut handles = BTreeSet::from([f.name.clone()]);
        if kind == Kind::Delegating {
            // What the accessor delegates TO is a handle as well: a test that
            // takes `posture_test_lock()` directly instead of through
            // `health_lock()` holds the same lock. Only lock-shaped call names
            // (`…lock`) — the accessor may also call a reset helper, and a
            // test calling only that is exactly the unlocked shape.
            for c in &f.facts.calls {
                if !c.contains("::") && c.ends_with("lock") {
                    handles.insert(c.clone());
                }
            }
        }
        serializers.push(Serializer {
            module: f.module.clone(),
            label: f.name.clone(),
            line: f.line,
            kind,
            handles,
            why,
        });
    }

    // Statics declared inside a `#[test]` fn's own body.
    for f in fns.iter().filter(|f| f.is_test) {
        for (name, line) in &f.facts.local_statics {
            serializers.push(Serializer {
                module: f.module.clone(),
                label: name.clone(),
                line: *line,
                kind: Kind::ScopedToOneTest,
                handles: BTreeSet::new(),
                why: Some(format!("declared inside test fn {}", f.name)),
            });
        }
    }

    // Population verdicts.
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in fns.iter().enumerate() {
        by_key.entry(f.key.as_str()).or_default().push(i);
    }
    let mut findings = Vec::new();
    for s in &serializers {
        match s.kind {
            Kind::CrossModule => continue,
            Kind::ScopedToOneTest => {
                findings.push(Finding {
                    serializer: s.clone(),
                    population: 0,
                    missing: Vec::new(),
                });
                continue;
            }
            Kind::ModuleStatic | Kind::FnLocal | Kind::Delegating => {}
        }
        let mut takes: Vec<bool> = fns
            .iter()
            .map(|f| f.facts.idents.iter().any(|i| s.handles.contains(i)))
            .collect();
        // Close over same-file calls. Monotone, so this settles.
        loop {
            let mut changed = false;
            for (i, f) in fns.iter().enumerate() {
                if !takes[i] && reaches(&f.facts, i, &by_key, &takes) {
                    takes[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut population = 0usize;
        let mut missing = Vec::new();
        for (i, f) in fns.iter().enumerate() {
            if !f.is_test || !in_module(&f.module, &s.module) {
                continue;
            }
            population += 1;
            if !takes[i] {
                missing.push(MissingTest {
                    line: f.line,
                    name: f.name.clone(),
                });
            }
        }
        missing.sort();
        if !missing.is_empty() {
            findings.push(Finding {
                serializer: s.clone(),
                population,
                missing,
            });
        }
    }

    Ok(FileReport {
        serializers,
        findings,
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ModuleStatic => "module-static",
        Kind::FnLocal => "fn-local",
        Kind::Delegating => "delegating",
        Kind::ScopedToOneTest => "scoped-to-one-test",
        Kind::CrossModule => "cross-module",
    }
}

fn render_finding(rel: &str, f: &Finding) -> String {
    let s = &f.serializer;
    let module = if s.module.is_empty() {
        "<file root>"
    } else {
        s.module.as_str()
    };
    match s.kind {
        Kind::ScopedToOneTest => format!(
            "src/{rel}:{} module `{module}` serialiser `{}` ({}): {} — only that test can \
             take it, so it serialises nothing",
            s.line,
            s.label,
            kind_name(s.kind),
            s.why.as_deref().unwrap_or("")
        ),
        _ => {
            let handles: Vec<String> = s.handles.iter().map(|h| format!("`{h}`")).collect();
            let missing: Vec<String> = f
                .missing
                .iter()
                .map(|m| format!("      src/{rel}:{} {}", m.line, m.name))
                .collect();
            format!(
                "src/{rel}:{} module `{module}` serialiser `{}` ({}, taken by {}): {} of {} \
                 test(s) in the module do not take it:\n{}",
                s.line,
                s.label,
                kind_name(s.kind),
                handles.join(" / "),
                f.missing.len(),
                f.population,
                missing.join("\n")
            )
        }
    }
}

/// What the allowlist says about one finding.
#[derive(Debug, PartialEq, Eq)]
enum AllowlistVerdict {
    NotListed,
    /// The entry at this index matches, and its expected set is exactly the
    /// finding's missing set.
    Matches(usize),
    /// The entry at this index matches the serialiser, but the tests outside
    /// the lock are not the ones it names — a test was added (or renamed)
    /// outside the lock, or one was fixed and the entry is behind.
    SetDiffers {
        ix: usize,
        message: String,
    },
}

/// Match a finding against an allowlist by `(file, module, serialiser)`, then
/// by the EXACT missing set. Pure, so the fixture test drives it with a
/// synthetic list.
fn allowlist_verdict_in(entries: &[AllowlistEntry], rel: &str, f: &Finding) -> AllowlistVerdict {
    let Some(ix) = entries.iter().position(|e| {
        e.file == rel && e.module == f.serializer.module && e.serializer == f.serializer.label
    }) else {
        return AllowlistVerdict::NotListed;
    };
    let entry = &entries[ix];
    let expected: BTreeSet<&str> = entry.outside_the_lock.iter().copied().collect();
    let actual: BTreeSet<&str> = f.missing.iter().map(|m| m.name.as_str()).collect();
    if expected == actual {
        return AllowlistVerdict::Matches(ix);
    }
    let added: Vec<String> = f
        .missing
        .iter()
        .filter(|m| !expected.contains(m.name.as_str()))
        .map(|m| format!("src/{rel}:{} {}", m.line, m.name))
        .collect();
    let gone: Vec<&str> = expected
        .iter()
        .filter(|n| !actual.contains(*n))
        .copied()
        .collect();
    let module = if f.serializer.module.is_empty() {
        "<file root>"
    } else {
        f.serializer.module.as_str()
    };
    let mut message = String::new();
    if !added.is_empty() {
        message.push_str(&format!(
            "a test was added outside `{}` in `{module}` (src/{rel}): {}; take the lock, or \
             extend the ALLOWLIST entry with the reason it never reaches the resource the \
             entry names ({})",
            f.serializer.label,
            added.join(", "),
            entry.reason
        ));
    }
    if !gone.is_empty() {
        if !message.is_empty() {
            message.push_str("\n  ");
        }
        message.push_str(&format!(
            "the ALLOWLIST entry for `{}` in `{module}` (src/{rel}) names {} test(s) that no \
             longer stand outside the lock: {} — remove them from `outside_the_lock`",
            f.serializer.label,
            gone.len(),
            gone.join(", ")
        ));
    }
    AllowlistVerdict::SetDiffers { ix, message }
}

fn allowlist_verdict(rel: &str, f: &Finding) -> AllowlistVerdict {
    allowlist_verdict_in(ALLOWLIST, rel, f)
}

// ---------------------------------------------------------------------------
// The guard
// ---------------------------------------------------------------------------

#[test]
fn every_test_in_a_module_with_a_serializer_takes_it_or_the_module_is_allowlisted() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rs_files(&root);
    assert!(
        files.len() > MIN_FILES_WALKED,
        "walked only {} .rs files under {} — the guard scanned nothing",
        files.len(),
        root.display()
    );

    let mut enumerated: Vec<String> = Vec::new();
    let mut serializer_count = 0usize;
    let mut violations: Vec<String> = Vec::new();
    let mut allowlist_hits: BTreeSet<usize> = BTreeSet::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/");
        let src =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("reading src/{rel}: {e}"));
        // A serialiser needs a unit mutex or a unit guard somewhere in the text.
        if !(src.contains("Mutex<()>") || src.contains("MutexGuard")) {
            continue;
        }
        let report = scan_source(&src, &rel).unwrap_or_else(|e| {
            panic!(
                "src/{rel}:{}: could not parse with syn ({e}) — the serialiser guard cannot \
                 vouch for a file it cannot read, so it refuses rather than skipping it",
                e.span().start().line
            )
        });
        for s in &report.serializers {
            serializer_count += 1;
            let module = if s.module.is_empty() {
                "<file root>"
            } else {
                s.module.as_str()
            };
            enumerated.push(format!(
                "  {:<19} src/{rel}:{} {module}::{}{}",
                kind_name(s.kind),
                s.line,
                s.label,
                s.why
                    .as_ref()
                    .map(|w| format!("  ({w})"))
                    .unwrap_or_default()
            ));
        }
        for f in &report.findings {
            match allowlist_verdict(&rel, f) {
                AllowlistVerdict::NotListed => violations.push(render_finding(&rel, f)),
                AllowlistVerdict::Matches(ix) => {
                    allowlist_hits.insert(ix);
                }
                AllowlistVerdict::SetDiffers { ix, message } => {
                    allowlist_hits.insert(ix);
                    violations.push(message);
                }
            }
        }
    }

    // Printed, not just compared, so the doc figure beside MIN_SERIALIZERS
    // stays verifiable: `cargo test -- --nocapture <this test>` re-measures it.
    eprintln!("test serialisers enumerated: {serializer_count} (floor >{MIN_SERIALIZERS})");
    for line in &enumerated {
        eprintln!("{line}");
    }
    assert!(
        serializer_count > MIN_SERIALIZERS,
        "found only {serializer_count} serialiser(s) — the detector has stopped recognising \
         them, so an empty finding list below would prove nothing:\n{}",
        enumerated.join("\n")
    );

    let stale: Vec<String> = ALLOWLIST
        .iter()
        .enumerate()
        .filter(|(ix, _)| !allowlist_hits.contains(ix))
        .map(|(_, e)| {
            format!(
                "  src/{} module `{}` serialiser `{}`",
                e.file, e.module, e.serializer
            )
        })
        .collect();
    assert!(
        stale.is_empty(),
        "{} ALLOWLIST entr{} in opt_in_serializer_guard.rs match no finding — the module was \
         fixed, renamed or moved, so the entry (and its reason) is stale. Remove it:\n{}",
        stale.len(),
        if stale.len() == 1 { "y" } else { "ies" },
        stale.join("\n")
    );

    assert!(
        violations.is_empty(),
        "{} module(s) define a test serialiser that only some of their tests take:\n  {}\n\n\
         A module-local `static … Mutex<()>` — or a `fn …_lock()` returning a guard over one — \
         serialises ONLY the tests that take it; the ones that do not run in parallel with all \
         of them, and it is one of the LOCKED tests that goes red, at random, with a panic \
         naming an assertion rather than the lock. Fix: take the serialiser at the top of every \
         test in the module (`let _g = <serialiser>;`), or — better — give the shared state a \
         per-test handle so there is nothing to serialise (the `LaneTable` shape, \
         `wedge_diagnostics.rs`). If the tests outside the lock genuinely never reach the \
         resource it guards, add the module to `ALLOWLIST` in opt_in_serializer_guard.rs with \
         that reason, where the nightly interleave census can check it. \
         Plan `2026-09-17-runner-tests-share-in-process-mutable-state`, Phase 5; dossier \
         `runner-tests-share-in-process-mutable-state`.",
        violations.len(),
        violations.join("\n  ")
    );
}

/// The guard must be SEEN to fail: a module with a fn-local serialiser two
/// tests take and one does not is named, with the module, the serialiser and
/// the missing test; a module-level static works the same; every excluded
/// shape is enumerated with its kind and asserted about nothing.
#[test]
fn the_guard_names_the_module_the_serializer_and_the_tests_that_do_not_take_it() {
    const SRC: &str = r#"
static PRODUCTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn crate_wide_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
static FILE_LEVEL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn series_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn takes_it_directly() {
        let _serialised = series_lock();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn takes_it_through_a_helper() {
        with_series(|| {});
    }

    fn with_series(f: impl FnOnce()) {
        let _g = series_lock();
        f();
    }

    #[test]
    fn takes_it_inside_a_macro() {
        assert!({
            let _g = series_lock();
            true
        });
    }

    #[test]
    fn forgets_it() {
        assert_eq!(1, 1);
    }

    mod nested {
        #[test]
        fn forgets_it_in_a_nested_module() {}
    }
}

#[cfg(test)]
mod static_tests {
    use once_cell::sync::Lazy;
    use std::sync::Mutex as StdMutex;

    static SERIAL: Lazy<StdMutex<()>> = Lazy::new(|| StdMutex::new(()));

    #[test]
    fn locks_the_static() {
        let _g = SERIAL.lock().unwrap();
    }
    #[test]
    fn forgets_the_static() {}
}

#[cfg(test)]
mod tokio_tests {
    static PG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn awaits_the_lock() {
        let _g = PG_TEST_LOCK.lock().await;
    }
    #[tokio::test]
    async fn forgets_the_tokio_lock() {}
}

#[cfg(test)]
mod delegating_tests {
    fn health_lock() -> std::sync::MutexGuard<'static, ()> {
        super::crate_wide_lock()
    }
    #[test]
    fn takes_the_delegate() {
        let _g = health_lock();
    }
    #[test]
    fn takes_the_delegates_target_directly() {
        let _g = super::crate_wide_lock();
    }
    #[test]
    fn forgets_the_delegate() {}
}

#[cfg(test)]
mod accessor_tests {
    static STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn store_lock() -> std::sync::MutexGuard<'static, ()> {
        STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    struct Handle(std::sync::MutexGuard<'static, ()>);
    impl Handle {
        fn set() -> Self {
            Handle(STORE_LOCK.lock().unwrap())
        }
    }
    #[test]
    fn takes_the_accessor() {
        let _g = store_lock();
    }
    #[test]
    fn takes_the_static() {
        let _g = STORE_LOCK.lock().unwrap();
    }
    #[test]
    fn takes_it_through_a_handle() {
        let _h = Handle::set();
    }
    #[test]
    fn forgets_the_accessor() {}
}

#[cfg(test)]
mod complete_tests {
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap()
    }
    #[test]
    fn one() { let _g = lock(); }
    #[test]
    fn two() { let _g = lock(); }
}

#[cfg(test)]
mod shared_tests {
    pub(super) fn shared_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap()
    }
    #[test]
    fn forgets_the_shared_lock() {}
}

#[cfg(test)]
mod internal_tests {
    fn capture_once(f: &impl Fn()) -> String {
        static CAPTURE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = CAPTURE_SERIAL.lock().unwrap();
        f();
        String::new()
    }
    #[test]
    fn uses_the_helper() { capture_once(&|| {}); }
    #[test]
    fn does_not() {}
}

#[cfg(test)]
mod one_test_tests {
    #[test]
    fn serialises_against_nothing() {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = SERIAL.lock().unwrap();
    }
    #[test]
    fn a_sibling() {}
}

#[cfg(test)]
mod data_lock_tests {
    static TABLE: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    fn table() -> std::sync::MutexGuard<'static, Vec<u8>> { TABLE.lock().unwrap() }
    #[test]
    fn not_a_serialiser() { table().push(1); }
}
"#;
    let report = scan_source(SRC, "fixture.rs").expect("synthetic source parses");

    let kinds: BTreeMap<String, (Kind, String)> = report
        .serializers
        .iter()
        .map(|s| {
            (
                format!("{}::{}", s.module, s.label),
                (s.kind, s.why.clone().unwrap_or_default()),
            )
        })
        .collect();
    let expect = [
        ("::crate_wide_lock", Kind::CrossModule),
        ("::FILE_LEVEL_LOCK", Kind::CrossModule),
        ("tests::series_lock", Kind::FnLocal),
        ("static_tests::SERIAL", Kind::ModuleStatic),
        ("tokio_tests::PG_TEST_LOCK", Kind::ModuleStatic),
        ("delegating_tests::health_lock", Kind::Delegating),
        ("accessor_tests::STORE_LOCK", Kind::ModuleStatic),
        ("complete_tests::lock", Kind::FnLocal),
        ("shared_tests::shared_lock", Kind::CrossModule),
        ("one_test_tests::SERIAL", Kind::ScopedToOneTest),
    ];
    for (label, kind) in expect {
        let got = kinds
            .get(label)
            .unwrap_or_else(|| panic!("serialiser `{label}` was not enumerated; got {kinds:?}"));
        assert_eq!(
            got.0, kind,
            "serialiser `{label}` has the wrong kind ({})",
            got.1
        );
    }
    assert_eq!(
        report.serializers.len(),
        expect.len(),
        "exactly these serialisers and no others — `PRODUCTION_LOCK` (not test-gated), \
         `CAPTURE_SERIAL` (internal to a helper that returns no guard), `TABLE` (a data \
         lock) and `accessor_tests::store_lock` (an accessor of STORE_LOCK, not a second \
         serialiser) must not appear: {:?}",
        report
            .serializers
            .iter()
            .map(|s| format!("{}::{}", s.module, s.label))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        kinds["shared_tests::shared_lock"].1, "accessor is pub(super)",
        "the cross-module reason names the visibility"
    );

    let by_label: BTreeMap<String, &Finding> = report
        .findings
        .iter()
        .map(|f| {
            (
                format!("{}::{}", f.serializer.module, f.serializer.label),
                f,
            )
        })
        .collect();
    let missing_of = |label: &str| -> Vec<&str> {
        by_label
            .get(label)
            .unwrap_or_else(|| panic!("no finding for `{label}`; findings: {:?}", by_label.keys()))
            .missing
            .iter()
            .map(|m| m.name.as_str())
            .collect()
    };
    assert_eq!(
        missing_of("tests::series_lock"),
        ["forgets_it", "forgets_it_in_a_nested_module"],
        "direct, helper and macro takers are credited; the two forgetters (one in a nested \
         module) are named"
    );
    assert_eq!(by_label["tests::series_lock"].population, 5);
    assert_eq!(missing_of("static_tests::SERIAL"), ["forgets_the_static"]);
    assert_eq!(
        missing_of("tokio_tests::PG_TEST_LOCK"),
        ["forgets_the_tokio_lock"]
    );
    assert_eq!(
        missing_of("delegating_tests::health_lock"),
        ["forgets_the_delegate"],
        "a test taking the lock the accessor delegates to (crate_wide_lock) directly is credited"
    );
    assert_eq!(by_label["delegating_tests::health_lock"].population, 3);
    assert_eq!(
        missing_of("accessor_tests::STORE_LOCK"),
        ["forgets_the_accessor"],
        "the static, its accessor fn and an RAII handle's path call all credit a test"
    );
    assert_eq!(
        by_label["one_test_tests::SERIAL"].population, 0,
        "a scoped-to-one-test finding carries no population — its kind is the finding"
    );
    assert!(
        !by_label.contains_key("complete_tests::lock"),
        "a module whose every test takes its serialiser is not a finding"
    );
    assert!(
        !by_label.contains_key("shared_tests::shared_lock")
            && !by_label.contains_key("::crate_wide_lock"),
        "cross-module serialisers are enumerated, never findings"
    );
    assert_eq!(
        report.findings.len(),
        6,
        "exactly the six findings above: {:?}",
        by_label.keys().collect::<Vec<_>>()
    );

    // The message is actionable: file, line, module, serialiser, the missing
    // tests with their lines — and the line is the fn's own.
    let text = render_finding("fixture.rs", by_label["tests::series_lock"]);
    let forgets_line = SRC
        .lines()
        .position(|l| l.contains("fn forgets_it()"))
        .expect("fixture line")
        + 1;
    for needle in [
        "src/fixture.rs:",
        "module `tests`",
        "serialiser `series_lock`",
        "fn-local",
        "2 of 5 test(s)",
        &format!("src/fixture.rs:{forgets_line} forgets_it"),
        "forgets_it_in_a_nested_module",
    ] {
        assert!(
            text.contains(needle),
            "finding text lacks {needle:?}:\n{text}"
        );
    }
    let scoped = render_finding("fixture.rs", by_label["one_test_tests::SERIAL"]);
    assert!(
        scoped.contains("scoped-to-one-test") && scoped.contains("serialises_against_nothing"),
        "{scoped}"
    );
}

/// `#[cfg(not(test))]` is the production arm: a unit-mutex static under it is
/// not a test serialiser, and a module under it is not a test module — while
/// `test` beside a `not(…)` still counts.
#[test]
fn cfg_not_test_is_production_not_a_test_module() {
    const SRC: &str = r#"
#[cfg(not(test))]
mod production {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> { LOCK.lock().unwrap() }
}
#[cfg(not(test))]
static PROD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(all(not(test), feature = "x"))]
mod also_production {
    static LOCK2: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
#[cfg(any(test, not(debug_assertions)))]
mod test_or_release {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn forgets() {}
}
"#;
    let report = scan_source(SRC, "x.rs").expect("parses");
    let labels: Vec<String> = report
        .serializers
        .iter()
        .map(|s| format!("{}::{}", s.module, s.label))
        .collect();
    assert_eq!(
        labels,
        ["test_or_release::SERIAL"],
        "only the module whose cfg names `test` outside a not(…) is a test module"
    );
    let attr_of = |src: &str| -> bool {
        let m: syn::ItemMod = syn::parse_str(&format!("{src} mod m {{}}")).expect("parses");
        is_cfg_test_attr(&m.attrs[0])
    };
    assert!(!attr_of("#[cfg(not(test))]"));
    assert!(attr_of("#[cfg(test)]"));
    assert!(attr_of("#[cfg(any(test, not(debug_assertions)))]"));
    assert!(!attr_of("#[cfg(all(not(test), feature = \"x\"))]"));
    assert!(
        !attr_of("#[cfg(feature = \"test-fixtures\")]"),
        "a string is not the ident"
    );
}

/// An allowlist entry names the EXACT tests outside the lock: a module that
/// gains one more unlocked test is named, one whose listed test got fixed is
/// named the other way, and a module not listed at all is a plain finding.
#[test]
fn an_allowlisted_module_that_gains_an_unlocked_test_is_named() {
    const SRC: &str = r#"
#[cfg(test)]
mod tests {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn takes_it() { let _g = SERIAL.lock().unwrap(); }
    #[test]
    fn known_outside() {}
    #[test]
    fn added_yesterday_outside() {}
}
"#;
    let report = scan_source(SRC, "m.rs").expect("parses");
    assert_eq!(report.findings.len(), 1);
    let f = &report.findings[0];
    let entry = |outside: &'static [&'static str]| AllowlistEntry {
        file: "m.rs",
        module: "tests",
        serializer: "SERIAL",
        outside_the_lock: outside,
        reason: "guards THE_RESOURCE; the tests outside it never touch it",
    };

    // Exact set → accepted.
    assert_eq!(
        allowlist_verdict_in(
            &[entry(&["known_outside", "added_yesterday_outside"])],
            "m.rs",
            f
        ),
        AllowlistVerdict::Matches(0)
    );
    // A test the entry does not name → named, with the fix and the reason.
    match allowlist_verdict_in(&[entry(&["known_outside"])], "m.rs", f) {
        AllowlistVerdict::SetDiffers { ix, message } => {
            assert_eq!(ix, 0);
            let added_line = SRC
                .lines()
                .position(|l| l.contains("fn added_yesterday_outside"))
                .expect("fixture line")
                + 1;
            for needle in [
                "a test was added outside `SERIAL` in `tests` (src/m.rs)",
                &format!("src/m.rs:{added_line} added_yesterday_outside"),
                "take the lock, or extend the ALLOWLIST entry",
                "guards THE_RESOURCE",
            ] {
                assert!(
                    message.contains(needle),
                    "message lacks {needle:?}:\n{message}"
                );
            }
            assert!(
                !message.contains("known_outside,") && !message.contains(" known_outside;"),
                "the already-listed test is not reported as added:\n{message}"
            );
        }
        other => panic!("expected SetDiffers, got {other:?}"),
    }
    // A listed test that now takes the lock → named the other way.
    match allowlist_verdict_in(
        &[entry(&[
            "known_outside",
            "added_yesterday_outside",
            "since_fixed",
        ])],
        "m.rs",
        f,
    ) {
        AllowlistVerdict::SetDiffers { message, .. } => {
            assert!(
                message
                    .contains("names 1 test(s) that no longer stand outside the lock: since_fixed"),
                "{message}"
            );
            assert!(!message.contains("a test was added"), "{message}");
        }
        other => panic!("expected SetDiffers, got {other:?}"),
    }
    // Not listed at all.
    assert_eq!(
        allowlist_verdict_in(&[], "m.rs", f),
        AllowlistVerdict::NotListed
    );
    assert_eq!(
        allowlist_verdict_in(&[entry(&["known_outside"])], "other.rs", f),
        AllowlistVerdict::NotListed,
        "the key is the file too"
    );
}

/// A test file's root is a test module: a `tests.rs` split out of its parent
/// (`spec_api/tests.rs`) defines its serialisers at file level, and they are
/// module-local, not cross-module.
#[test]
fn a_test_files_root_counts_as_a_test_module() {
    const SRC: &str = r#"
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn takes() { let _g = SERIAL.lock().unwrap(); }
#[test]
fn forgets() {}
"#;
    let report = scan_source(SRC, "spec_api/tests.rs").expect("parses");
    assert_eq!(report.serializers.len(), 1);
    assert_eq!(report.serializers[0].kind, Kind::ModuleStatic);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].missing[0].name, "forgets");
    assert_eq!(report.findings[0].population, 2);

    let as_ordinary = scan_source(SRC, "spec_api/mod.rs").expect("parses");
    assert!(
        as_ordinary.serializers.is_empty(),
        "the same static in a non-test file is production, not a serialiser"
    );
    assert!(
        is_test_file("foo/bar_tests.rs")
            && is_test_file("tests/x.rs")
            && !is_test_file("foo/tests_helper.rs")
    );
}

/// A file that does not parse is an error the real-tree test turns into a
/// named failure — never a silent skip.
#[test]
fn the_guard_refuses_source_it_cannot_parse() {
    assert!(scan_source(
        "#[cfg(test)] mod tests { static LOCK: Mutex<()> = ; }",
        "x.rs"
    )
    .is_err());
}
