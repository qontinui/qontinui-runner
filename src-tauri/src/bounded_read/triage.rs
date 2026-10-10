//! The Phase 6 triage of every runner `limit` site — one row per
//! [`super::ReadLimit`] constant, checked in so the census test can hold the
//! table and the source to each other.
//!
//! Plan `2026-09-05-every-bounded-read-is-a-page-that-reads-as-a-corpus`,
//! Phase 6, first task: "sequenced by whether a site backs an agent-facing
//! HTTP door or only the runner's own UI — that triage has not been done".
//! This is that triage. It was taken against qontinui-runner `origin/main`
//! with qontinui-runner#2100 stacked under it, by tracing each site from the
//! axum routers `mcp_api.rs` merges (and the `POST /graphql` resolvers) down to
//! the statement, and each Tauri command to its `invoke` callers.
//!
//! **`mcp_api.rs` applies no bound of its own.** It is the router: it merges
//! each module's `routes()` (`crate::mcp::*`, `spec_api`, `trace_api`,
//! `state_discovery`, the GraphQL sub-router) and every `limit` is resolved
//! in the handler those routes name, or in the database function the handler
//! calls. So the :9876 door's bounds are exactly the [`Door::Agent`] rows
//! below; the router is neither unbounded nor a second place to look.
//!
//! The census (`super::tests`) fails when:
//! - a `limit…unwrap_or(` (or a stringly `.get("limit")…unwrap_or(`) appears
//!   anywhere outside [`NOT_A_BOUND`] — every bound goes through a
//!   `ReadLimit`;
//! - a `ReadLimit` constant has no row here, or a row names a constant that no
//!   longer exists;
//! - a row in an agent-door directory is not [`Door::Agent`];
//! - an [`Disclosure::Envelope`] row's resolving function serves no
//!   `BoundedReadMeta`, or a [`Disclosure::Pending`] one now does (flip it);
//! - the [`Disclosure::Pending`] count rises above
//!   [`UNDISCLOSED_AGENT_SITES_CEILING`] — the shrinking allowlist.

/// Who can reach the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// Class A: an agent-facing door — the :9876 HTTP API (any route merged by
    /// `mcp_api.rs`, `POST /graphql`, `POST /query`) or the embedded MCP
    /// server. Severity is high: an agent reads these and acts on what it
    /// sees.
    Agent,
    /// Class B: a Tauri command only the runner's own React UI invokes.
    RunnerUi,
}

/// What the read tells its caller about the bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disclosure {
    /// The response carries the shared `BoundedReadMeta` keys (and, for a
    /// walk, a keyset `next_cursor`).
    Envelope,
    /// Class A, clamped but NOT yet disclosed — the shrinking allowlist. The
    /// string says what the disclosure needs (a keyset walk, an
    /// `enumerate_via` door for a ranking, …).
    Pending(&'static str),
    /// No disclosure owed: a runner-UI read whose bound is named and clamped
    /// (class B), or a bound that limits work rather than a returned list.
    NotOwed(&'static str),
}

/// One `ReadLimit` constant.
#[derive(Debug, Clone, Copy)]
pub struct Site {
    /// The file declaring the constant, relative to `src-tauri/`.
    pub file: &'static str,
    /// The constant's name.
    pub limit: &'static str,
    pub door: Door,
    pub disclosure: Disclosure,
    /// The read, as its caller reaches it.
    pub read: &'static str,
}

/// The most [`Disclosure::Pending`] rows the table may hold. **Lower it when a
/// door starts disclosing; never raise it.** A new agent-facing read lands
/// disclosed or not at all.
pub const UNDISCLOSED_AGENT_SITES_CEILING: usize = 36;

/// Files that spell `limit…unwrap_or(` for something that is not a read bound.
/// Each entry is `(file, why)`.
pub const NOT_A_BOUND: &[(&str, &str)] = &[(
    "src/plan_workunit_adapter/push.rs",
    "reads the `limit` coord ECHOED in a response body (the cap coord applied), to detect a \
     clamped page; it bounds nothing",
)];

const WALK: &str = "a walk over an immutable timestamp with uuid ids: keyset it and serve \
                    BoundedReadMeta";
const WALK_OFFSET: &str = "pages with OFFSET today: move it to a keyset walk and serve \
                           BoundedReadMeta";
const RANKING: &str = "a relevance ranking that cannot page: serve BoundedReadMeta with an \
                       enumerate_via door";
const AGGREGATE: &str = "a top-N over GROUP BY: needs a probe (limit + 1) and an enumerate_via \
                         door, or a keyset over the group key";

const RUNNER_UI: Disclosure =
    Disclosure::NotOwed("runner-UI read; the named, clamped constant is the fix it was owed");

pub const TRIAGE: &[Site] = &[
    // ---------------------------------------------------------------- class A
    Site {
        file: "src/commands/mcp.rs",
        limit: "MCP_CALLS_PAGE",
        door: Door::Agent,
        disclosure: Disclosure::Envelope,
        read: "GET /task-runs/{id}/mcp-calls and Tauri get_task_run_mcp_calls: keyset over \
               task_run_mcp_calls (created_at, id) ASC",
    },
    Site {
        file: "src/database/pg/error_monitor.rs",
        limit: "ERROR_EVENTS_QUERY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "error_events orders by last_seen_at, which every recurrence updates: needs an \
             immutable walk key before it can page",
        ),
        read: "POST /graphql errorEvents, POST /query error_events, Tauri error-monitor commands",
    },
    Site {
        file: "src/mcp/automation_runs.rs",
        limit: "AUTOMATION_RUNS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /runs: task_run_automation by started_at DESC",
    },
    Site {
        file: "src/mcp/error_monitor.rs",
        limit: "UNRESOLVED_ERRORS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "unresolved error events order by a mutable last_seen_at: needs an immutable walk \
             key",
        ),
        read: "GET /error-monitor/errors",
    },
    Site {
        file: "src/mcp/file_registry.rs",
        limit: "HEATMAP_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(AGGREGATE),
        read: "GET /file-activity/heatmap: top files / sessions by distinct-toucher count over a \
               window of the mutable recorded_at",
    },
    Site {
        file: "src/mcp/findings_api.rs",
        limit: "FINDINGS_PAGE_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK_OFFSET),
        read: "GET /findings and GET /findings/by-status/{status}: task_run_findings by \
               detected_at DESC, `page` → OFFSET",
    },
    Site {
        file: "src/mcp/graph_api.rs",
        limit: "GRAPH_SEARCH_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(RANKING),
        read: "GET /graph/search: multi-source ILIKE, scored and truncated in memory",
    },
    Site {
        file: "src/mcp/graph_api.rs",
        limit: "MEMORY_SEARCH_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(RANKING),
        read: "GET /memory/search: fused multi-source ranking",
    },
    Site {
        file: "src/mcp/graph_api.rs",
        limit: "SIMILAR_ERRORS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(RANKING),
        read: "GET /graph/similar-errors: trigram similarity ranking",
    },
    Site {
        file: "src/mcp/inngest.rs",
        limit: "EVENT_HISTORY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "an in-memory ring buffer of at most 500 events: serve BoundedReadMeta from a \
             limit + 1 read of the buffer",
        ),
        read: "GET /inngest/events",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "AGENT_TRACE_AGGREGATES_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "the pipeline_traces source is a stub that returns no rows: delete the door or give \
             it a store, then disclose",
        ),
        read: "GET /meta-optimizer/agent-trace-aggregates",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "BEAM_RUNS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/beam-runs: beam_search_runs by created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "CANARY_HISTORY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/canaries/history: completed canary rollouts",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "DUEL_POOLS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/duel-pools: duel_pools by created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "DUEL_RESULTS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/duel-pools/{id}/results: duel_results by created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "GENERATION_FEEDBACK_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /workflow-generation/feedback L1/L2: workflow_generation_feedback by \
               created_at DESC (LIMIT interpolated into the SQL)",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "ITERATION_HISTORY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/iteration-history: task_runs with iteration history by \
               created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "LEARNING_OUTCOMES_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /learning/outcomes L1/L2: learning_outcomes by created_at DESC (LIMIT \
               interpolated into the SQL)",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "PROMPT_EVOLUTION_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/prompt-evolution: prompt_evolution by created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "REFLECTION_FIXES_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/reflection-fixes L1/L2: reflection_fixes by created_at DESC",
    },
    Site {
        file: "src/mcp/meta_optimizer_api.rs",
        limit: "SPAN_EVENTS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /meta-optimizer/span-events: span_events by created_at ASC, step_index",
    },
    Site {
        file: "src/mcp/misc.rs",
        limit: "DEBUG_ERRORS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "reads and sorts dev-log files in memory: serve BoundedReadMeta from the merged \
             count before truncation",
        ),
        read: "GET /debug/app/errors",
    },
    Site {
        file: "src/mcp/online_learning_api.rs",
        limit: "EXPERIENCES_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /online-learning/experiences: experience_summaries by created_at DESC",
    },
    Site {
        file: "src/mcp/online_learning_api.rs",
        limit: "STEP_SCORECARD_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(AGGREGATE),
        read: "GET /online-learning/step-scorecard: step types by average credit",
    },
    Site {
        file: "src/mcp/security_audit.rs",
        limit: "AUDIT_EVENTS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "orders by a TEXT `timestamp` column: walk the timestamptz created_at instead",
        ),
        read: "GET /security/audit/events",
    },
    Site {
        file: "src/mcp/state_explorer.rs",
        limit: "EXPLORATION_HISTORY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "lists report directories on disk: serve BoundedReadMeta from the directory count",
        ),
        read: "GET /state-explorer/history",
    },
    Site {
        file: "src/mcp/task_runs.rs",
        limit: "CHECKPOINTS_PAGE_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "already keyset on step_index with its own cursor: move it onto the shared codec \
             and BoundedReadMeta",
        ),
        read: "GET /task-runs/{id}/checkpoints",
    },
    Site {
        file: "src/mcp/task_runs.rs",
        limit: "EXECUTION_SPANS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "execution_spans has bigint ids and a TEXT start_ts: needs a (timestamptz, id) key \
             the shared codec can carry",
        ),
        read: "GET /execution-spans",
    },
    Site {
        file: "src/mcp/task_runs.rs",
        limit: "TASK_RUNS_LIST_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "orders by the MUTABLE updated_at (D8): keep that order for display only and walk \
             (created_at, id)",
        ),
        read: "GET /task-runs",
    },
    Site {
        file: "src/mcp/testing.rs",
        limit: "INTEGRATION_TEST_RUNS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET integration test runs",
    },
    Site {
        file: "src/mcp/token_analytics.rs",
        limit: "TASK_RUN_COSTS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(AGGREGATE),
        read: "GET /analytics/token-usage/task-runs: runs ranked by summed cost",
    },
    Site {
        file: "src/mcp/ui_bridge/elements.rs",
        limit: "CHANGES_SINCE_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "forwards `limit` to the UI Bridge SDK over IPC: the SDK's answer must carry the \
             bound back",
        ),
        read: "UI Bridge get_changes_since",
    },
    Site {
        file: "src/mcp/verification_tests.rs",
        limit: "TEST_HISTORY_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK),
        read: "GET /tests/history: status counts plus the newest test_results by created_at",
    },
    Site {
        file: "src/mcp_embedded.rs",
        limit: "RUNNER_LOGS_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "tails JSONL log files: serve BoundedReadMeta from the per-file line count",
        ),
        read: "embedded MCP tool read_runner_logs",
    },
    Site {
        file: "src/spec_api/proposals.rs",
        limit: "PROPOSALS_LIST_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(WALK_OFFSET),
        read: "GET /apps/{app_id}/spec/proposals (feature spec-authoring): spec_proposals by \
               created_at DESC, OFFSET",
    },
    Site {
        file: "src/spec_api/proposals.rs",
        limit: "SCAN_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::NotOwed(
            "bounds the WORK one POST /spec/proposals/scan does, not a returned list; the \
             response reports what it scanned",
        ),
        read: "POST /apps/{app_id}/spec/proposals/scan",
    },
    Site {
        file: "src/state_discovery/drift_scores.rs",
        limit: "DRIFT_SCORES_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(
            "state_discovery_drift_scores has BIGSERIAL ids: needs a key the shared codec can \
             carry",
        ),
        read: "GET /state-discovery/drift-scores",
    },
    Site {
        file: "src/trace_api/handlers.rs",
        limit: "TRACE_LIST_LIMIT",
        door: Door::Agent,
        disclosure: Disclosure::Pending(AGGREGATE),
        read: "GET /trace/list: recording sessions grouped from ui_bridge_events",
    },
    // ---------------------------------------------------------------- class B
    Site {
        file: "src/commands/adaptive_learning.rs",
        limit: "CURATED_EXAMPLES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_curated_examples",
    },
    Site {
        file: "src/commands/adaptive_learning.rs",
        limit: "GEPA_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_gepa_runs",
    },
    Site {
        file: "src/commands/adaptive_learning.rs",
        limit: "PLAYBOOK_ENTRIES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_playbook_entries",
    },
    Site {
        file: "src/commands/ai_data.rs",
        limit: "JSONL_VIEWER_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri read_jsonl_logs_for_viewer",
    },
    Site {
        file: "src/commands/ai_data.rs",
        limit: "TASK_RUN_EVENTS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_task_run_events_from_db",
    },
    Site {
        file: "src/commands/ai_data.rs",
        limit: "TASK_RUN_LOG_PAGE",
        door: Door::RunnerUi,
        disclosure: Disclosure::Envelope,
        read: "Tauri get_task_run_{playwright_results,api_requests,awas_steps}_from_db: keyset \
               (qontinui-runner#2100)",
    },
    Site {
        file: "src/commands/ai_data.rs",
        limit: "VIEWER_TASK_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_task_runs_for_viewer",
    },
    Site {
        file: "src/commands/checkpoint_browser.rs",
        limit: "CHECKPOINTS_PAGE",
        door: Door::RunnerUi,
        disclosure: Disclosure::Envelope,
        read: "Tauri get_checkpoints_paginated: keyset over orchestrator_checkpoints (created_at, \
               id)",
    },
    Site {
        file: "src/commands/checks.rs",
        limit: "CHECK_RESULTS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_check_results",
    },
    Site {
        file: "src/commands/deconflict.rs",
        limit: "OVERLAPPING_INTENTS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri list_overlapping_intents (a proxy to coord, which applies its own cap)",
    },
    Site {
        file: "src/commands/event_search.rs",
        limit: "EVENT_SEARCH_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri search_events",
    },
    Site {
        file: "src/commands/flow.rs",
        limit: "FLOW_EXECUTIONS_PAGE",
        door: Door::RunnerUi,
        disclosure: Disclosure::Envelope,
        read: "Tauri get_flow_executions_paginated: keyset over flow_executions (started_at, \
               instance_id)",
    },
    Site {
        file: "src/commands/learning.rs",
        limit: "LEARNING_OUTCOMES_PAGE",
        door: Door::RunnerUi,
        disclosure: Disclosure::Envelope,
        read: "Tauri get_learning_outcomes_paginated: keyset over learning_outcomes (created_at, \
               id)",
    },
    Site {
        file: "src/commands/learning.rs",
        limit: "RECENT_TASKS_WITH_OUTCOMES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_recent_tasks_with_outcomes",
    },
    Site {
        file: "src/commands/meta_optimizer.rs",
        limit: "AGENT_EFFECTIVENESS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_agent_effectiveness",
    },
    Site {
        file: "src/commands/meta_optimizer.rs",
        limit: "PROMPT_EVOLUTION_HISTORY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_prompt_evolution_history",
    },
    Site {
        file: "src/commands/rag.rs",
        limit: "RAG_SEMANTIC_SEARCH_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri search_rag_elements_semantic",
    },
    Site {
        file: "src/commands/shell_commands.rs",
        limit: "SHELL_COMMAND_RESULTS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_shell_command_results",
    },
    Site {
        file: "src/commands/state_explorer.rs",
        limit: "EXPLORATION_HISTORY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_exploration_history (GET /state-explorer/history is its own copy, \
               class A)",
    },
    Site {
        file: "src/commands/testing.rs",
        limit: "RECENT_TASK_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri list_recent_task_runs",
    },
    Site {
        file: "src/commands/tiered_info.rs",
        limit: "AI_SESSION_HISTORY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_ai_session_history",
    },
    Site {
        file: "src/commands/tiered_info.rs",
        limit: "FAILED_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_failed_runs",
    },
    Site {
        file: "src/commands/tiered_info.rs",
        limit: "RECENT_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_recent_runs (GET /runs calls the same statement with its own limit, \
               class A)",
    },
    Site {
        file: "src/commands/token_analytics.rs",
        limit: "TASK_RUN_COSTS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_task_run_costs",
    },
    Site {
        file: "src/database/pg/learning.rs",
        limit: "LEARNING_OUTCOMES_FILTERED_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_learning_outcomes_filtered",
    },
    Site {
        file: "src/database/pg/misc_crud.rs",
        limit: "ARTIFACTS_QUERY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "the ui-bridge Tauri plugin's query_artifacts (an invoke handler, not an HTTP \
               route)",
    },
    Site {
        file: "src/database/pg/misc_crud.rs",
        limit: "MOBILE_LOGS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_mobile_logs",
    },
    Site {
        file: "src/database/pg/misc_crud.rs",
        limit: "MOBILE_STATES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_mobile_states",
    },
    Site {
        file: "src/database/pg/regression.rs",
        limit: "RECENT_DIAGNOSES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri regression commands",
    },
    Site {
        file: "src/database/pg/regression.rs",
        limit: "REGRESSION_RUNS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri regression commands",
    },
    Site {
        file: "src/database/pg/regression.rs",
        limit: "REGRESSION_SUITES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri regression commands",
    },
    Site {
        file: "src/error_monitor/commands.rs",
        limit: "ERROR_SEARCH_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri search_errors",
    },
    Site {
        file: "src/error_monitor/commands.rs",
        limit: "RECENT_ERRORS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_recent_errors",
    },
    Site {
        file: "src/error_monitor/commands.rs",
        limit: "UNRESOLVED_ERRORS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_unresolved_errors (GET /error-monitor/errors has its own, class A)",
    },
    Site {
        file: "src/process_capture/commands.rs",
        limit: "PROCESS_LOG_SEARCH_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri search_process_logs",
    },
    Site {
        file: "src/process_capture/commands.rs",
        limit: "PROCESS_OUTPUT_LINES_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_process_session_output_from_db",
    },
    Site {
        file: "src/process_capture/commands.rs",
        limit: "PROCESS_SESSIONS_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_process_sessions_from_db",
    },
    Site {
        file: "src/spec_experimentation/compliance.rs",
        limit: "COMPLIANCE_HISTORY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_spec_compliance_history",
    },
    Site {
        file: "src/spec_experimentation/versioning.rs",
        limit: "VERSION_HISTORY_LIMIT",
        door: Door::RunnerUi,
        disclosure: RUNNER_UI,
        read: "Tauri get_spec_version_history",
    },
];
