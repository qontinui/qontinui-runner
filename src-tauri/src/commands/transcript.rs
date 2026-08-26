//! Tauri commands for Claude Code transcript import and workflow generation.
//!
//! Provides commands to list, read, and extract Claude Code session transcripts,
//! plus a standalone workflow generation command that doesn't require an existing task_run_id.

use crate::commands::compartments::{ExecutionCompartment, HealthCompartment, StorageCompartment};
use crate::commands::CommandResponse;
use crate::terminal::transcript;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Collect workspace project paths for scanning (root + immediate child directories).
fn collect_workspace_project_paths() -> Vec<String> {
    let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut paths = vec![workspace_root.clone()];
    if !workspace_root.is_empty() {
        if let Ok(entries) = std::fs::read_dir(&workspace_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if !name.starts_with('.') && name != "node_modules" {
                        paths.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    paths
}

/// Collect all transcript sessions across all project paths and config dirs, deduplicated.
fn collect_all_sessions(
    project_paths: &[String],
    config_dirs: &[std::path::PathBuf],
) -> Vec<transcript::TranscriptSession> {
    let mut all_sessions = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for project in project_paths {
        for dir in config_dirs {
            match transcript::list_sessions(dir, project) {
                Ok(sessions) => {
                    for session in sessions {
                        if seen_ids.insert(session.session_id.clone()) {
                            all_sessions.push(session);
                        }
                    }
                }
                Err(e) => warn!("Failed to list sessions in {:?}: {}", dir, e),
            }
        }
    }

    all_sessions.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    all_sessions
}

// ── Scan cache (single-flight + short TTL) ───────────────────────────────────
//
// **Why this exists.** Every terminal tab is its own `WebviewWindow`
// (`commands::terminal_windows`), so each one loads a full app instance with
// its own `useTranscriptSessions` hook and its own 30s poll. N tabs therefore
// meant N *independent* full-workspace scans, and because the tabs mount
// together their timers stay in phase — they fire as one burst.
//
// Measured on the fleet box 2026-08-07 with 6 tabs open (Terminal, tio, gmail,
// hotmail, paktis, qontinui):
//
// ```text
// 10:16:01.383 … .394   6 scans start  (11ms spread)
// 10:16:01.808 … .816   all 6 finish   (~433ms burst)
// ```
//
// Each scan crosses 624 workspace child directories with 6 Claude config dirs
// — ~3,744 `list_sessions` calls — so the burst was ~22,000 filesystem probes
// for six byte-identical answers. Worse, the work ran inline on the async
// command, occupying a runtime worker for its whole duration.
//
// Two independent growth axes made this worth fixing rather than tolerating:
// tabs open, and workspace child directories (441 of those 624 were leftover
// worktree/spawn dirs). Both scale the burst linearly.
//
// The fix is deliberately not a background refresher: holding the lock across
// the scan makes the *first* caller do the work while the other five await it
// and then read the fresh entry — one scan per burst, no staleness window
// beyond the TTL, and no timer to keep alive when nothing is watching.

/// How long a completed scan stays servable. Must be shorter than the
/// frontend's 30s poll (so each poll round still sees fresh data) and longer
/// than one burst's spread (so a burst collapses to a single scan).
const SCAN_CACHE_TTL: Duration = Duration::from_secs(5);

struct ScanCache {
    computed_at: Instant,
    /// Inputs the entry was computed for. A caller asking about different
    /// paths (e.g. `all_projects: false`) must not be served this entry.
    project_paths: Vec<String>,
    config_dirs: Vec<std::path::PathBuf>,
    sessions: Vec<transcript::TranscriptSession>,
}

fn scan_cache() -> &'static Mutex<Option<ScanCache>> {
    static CACHE: OnceLock<Mutex<Option<ScanCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Count of scans actually executed (cache misses). Lets a test prove the
/// collapse happened rather than inferring it from timing, and is a cheap
/// operational signal if it is ever surfaced.
static SCANS_PERFORMED: AtomicU64 = AtomicU64::new(0);

/// Number of real filesystem scans performed since process start.
pub fn scans_performed() -> u64 {
    SCANS_PERFORMED.load(Ordering::Relaxed)
}

/// True when a cache entry may serve a request for these inputs.
/// Pure — unit-tested without a clock or a filesystem.
fn entry_is_servable(
    age: Duration,
    entry_paths: &[String],
    entry_dirs: &[std::path::PathBuf],
    want_paths: &[String],
    want_dirs: &[std::path::PathBuf],
) -> bool {
    age < SCAN_CACHE_TTL && entry_paths == want_paths && entry_dirs == want_dirs
}

/// [`collect_all_sessions`] behind the single-flight cache, with the blocking
/// filesystem work moved off the async runtime.
///
/// The lock is held across the scan on purpose — that is what makes it
/// single-flight. Concurrent callers park on the mutex (cheap: no runtime
/// worker is held, the scan itself is on the blocking pool) and then read the
/// entry the winner just wrote.
async fn collect_all_sessions_cached(
    project_paths: &[String],
    config_dirs: &[std::path::PathBuf],
) -> Vec<transcript::TranscriptSession> {
    let mut guard = scan_cache().lock().await;

    if let Some(entry) = guard.as_ref() {
        if entry_is_servable(
            entry.computed_at.elapsed(),
            &entry.project_paths,
            &entry.config_dirs,
            project_paths,
            config_dirs,
        ) {
            debug!(
                "transcript_list_sessions: served {} sessions from scan cache",
                entry.sessions.len()
            );
            return entry.sessions.clone();
        }
    }

    let paths = project_paths.to_vec();
    let dirs = config_dirs.to_vec();
    SCANS_PERFORMED.fetch_add(1, Ordering::Relaxed);
    let sessions =
        match tokio::task::spawn_blocking(move || collect_all_sessions(&paths, &dirs)).await {
            Ok(s) => s,
            Err(e) => {
                // The blocking pool panicked or was shut down. Report empty rather
                // than caching the failure — the next tick retries.
                warn!("transcript_list_sessions: scan task failed: {e}");
                return Vec::new();
            }
        };

    *guard = Some(ScanCache {
        computed_at: Instant::now(),
        project_paths: project_paths.to_vec(),
        config_dirs: config_dirs.to_vec(),
        sessions: sessions.clone(),
    });
    sessions
}

/// List Claude Code transcript sessions.
///
/// When `all_projects` is true, scans the workspace root **and** all immediate
/// child directories (the individual repos in the monorepo) so sessions started
/// from subdirectories are visible too.
#[tauri::command]
pub async fn transcript_list_sessions(
    project_path: Option<String>,
    all_projects: Option<bool>,
) -> Result<CommandResponse, String> {
    let project_paths: Vec<String> = if all_projects.unwrap_or(true) {
        collect_workspace_project_paths()
    } else {
        let project = project_path.unwrap_or_else(|| {
            crate::mcp::shared::get_workspace_paths_internal()
                .map(|(root, _, _)| root.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        if project.is_empty() {
            return Ok(CommandResponse {
                success: false,
                message: Some("No project path available".to_string()),
                data: None,
            });
        }
        vec![project]
    };

    let config_dirs = transcript::find_claude_config_dirs();
    info!(
        "transcript_list_sessions: scanning {} project paths across {} config dirs",
        project_paths.len(),
        config_dirs.len()
    );

    // Note: don't early-return on empty config_dirs — Phase 5.1 fakes still
    // need to flow through even when the host has no Claude installations.
    let all_sessions = if config_dirs.is_empty() {
        Vec::new()
    } else {
        collect_all_sessions_cached(&project_paths, &config_dirs).await
    };

    // Phase 5.1 of the UI Bridge discoverability/effectiveness plan:
    // append any fakes injected via the debug-only
    // `/ui-bridge/test/inject-session` route so SessionCard can render
    // without a live PTY. The merge is a no-op (and the module + accessor
    // don't exist) on production release builds without `test-fixtures`.
    #[cfg(any(debug_assertions, feature = "test-fixtures"))]
    let all_sessions = crate::mcp::test_fixtures::merge_with_injected(
        all_sessions,
        crate::mcp::test_fixtures::injected_test_sessions(),
    );

    info!(
        "transcript_list_sessions: found {} total sessions",
        all_sessions.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} sessions", all_sessions.len())),
        data: Some(serde_json::to_value(&all_sessions).unwrap_or_default()),
    })
}

/// Read all messages from a specific Claude Code transcript session.
#[tauri::command]
pub async fn transcript_read_session(
    session_id: String,
    config_dir: Option<String>,
    project_path: Option<String>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // If config_dir provided, use it directly; otherwise scan all config dirs
    let config_dirs = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    for dir in &config_dirs {
        match transcript::read_session(dir, &project, &session_id) {
            Ok(messages) => {
                return Ok(CommandResponse {
                    success: true,
                    message: Some(format!("Read {} messages", messages.len())),
                    data: Some(serde_json::to_value(&messages).unwrap_or_default()),
                });
            }
            Err(_) => continue, // Try next config dir
        }
    }

    Ok(CommandResponse {
        success: false,
        message: Some(format!(
            "Session '{}' not found in any config directory",
            session_id
        )),
        data: None,
    })
}

/// Read ONLY the operator's own prompts from a session transcript.
///
/// Backs the per-zone prompts panel, which polls while open. Two properties
/// that `transcript_read_session` does not have and that a poll needs:
/// machine-authored `user` records (`isMeta` slash-command expansions,
/// `isCompactSummary` continuations, `isSidechain` subagent turns) are dropped
/// server-side where the flags still exist, and `sinceMtimeMs` skips the parse
/// entirely while the file is untouched. See `transcript::read_user_prompts`
/// for the measurements behind both.
#[tauri::command]
pub async fn transcript_read_user_prompts(
    session_id: String,
    config_dir: Option<String>,
    project_path: Option<String>,
    since_mtime_ms: Option<u64>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    let config_dirs = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    // Reading and parsing a transcript is blocking file I/O — on the largest
    // real transcript here, 10.4 MB. Keep it off the async worker.
    let result = tokio::task::spawn_blocking(move || {
        for dir in &config_dirs {
            if let Ok(r) =
                transcript::read_user_prompts(dir, &project, &session_id, since_mtime_ms)
            {
                return Some(r);
            }
        }
        None
    })
    .await
    .map_err(|e| format!("prompt read task failed: {e}"))?;

    match result {
        Some(r) => Ok(CommandResponse {
            success: true,
            message: Some(if r.unchanged {
                "Transcript unchanged".to_string()
            } else {
                format!("Read {} prompts", r.prompts.len())
            }),
            data: Some(serde_json::to_value(&r).unwrap_or_default()),
        }),
        None => Ok(CommandResponse {
            success: false,
            message: Some("Session transcript not found in any config directory".to_string()),
            data: None,
        }),
    }
}

/// Get the most recent Claude Code session for the current project.
///
/// `config_dir` filters to a specific Claude config directory (e.g.
/// `C:\claude\.claude-hotmail`). When omitted, all known config dirs are
/// scanned and the first hit wins. The filter is required for callers
/// like `useTabSessionIdCapture` that launched a tab into a specific
/// account — without it, a more-recently-touched JSONL in a *different*
/// account for the same `project_path` would shadow the session we
/// actually want.
///
/// `since_ms` (epoch milliseconds, matches `Date.now()` on the JS side as
/// camelCase `sinceMs`) filters out sessions whose `last_modified` is at
/// or before the supplied threshold. This lets the frontend lift its
/// freshness check into the backend so the `.claude.json` shortcut and
/// mtime fallback are filtered consistently — see Phase 1 of
/// `plans/traffic-light-session-id-followups.md`.
#[tauri::command]
pub async fn transcript_get_latest(
    project_path: Option<String>,
    config_dir: Option<String>,
    since_ms: Option<i64>,
) -> Result<CommandResponse, String> {
    let project = project_path.unwrap_or_else(|| {
        crate::mcp::shared::get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // If config_dir was supplied, scope to it; otherwise scan all known dirs.
    let config_dirs: Vec<std::path::PathBuf> = if let Some(dir) = config_dir {
        vec![std::path::PathBuf::from(dir)]
    } else {
        transcript::find_claude_config_dirs()
    };

    // Convert the epoch-millis threshold to a UTC timestamp for the backend
    // helper. `from_timestamp_millis` returns `None` for out-of-range
    // values; in that case we behave as if no filter was supplied.
    let since = since_ms.and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis);

    // Try each config dir, return first match
    for dir in &config_dirs {
        if let Some(session) = transcript::get_latest_session_id(dir, &project, since) {
            return Ok(CommandResponse {
                success: true,
                message: Some(format!("Latest session: {}", session.session_id)),
                data: Some(serde_json::to_value(&session).unwrap_or_default()),
            });
        }
    }

    Ok(CommandResponse {
        success: true,
        message: Some("No sessions found".to_string()),
        data: None,
    })
}

/// Compute lightweight digests for recent sessions (frozen detection + work summary hints).
///
/// Takes the most recent N sessions (default 50) and returns a digest for each,
/// reading only the tail of each JSONL file for efficiency.
#[tauri::command]
pub async fn transcript_session_digests(
    max_sessions: Option<usize>,
) -> Result<CommandResponse, String> {
    let project_paths = collect_workspace_project_paths();
    let config_dirs = transcript::find_claude_config_dirs();
    if config_dirs.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No config directories found".to_string()),
            data: Some(serde_json::json!([])),
        });
    }

    // Shares the single-flight scan cache with `transcript_list_sessions`:
    // this runs once per tab ON MOUNT, so six tabs previously meant six more
    // full scans stacked on top of the poll bursts during startup — the
    // busiest moment the runner has.
    let mut all_sessions = collect_all_sessions_cached(&project_paths, &config_dirs).await;
    let limit = max_sessions.unwrap_or(50).min(100);
    all_sessions.truncate(limit);

    // Digest computation reads the TAIL OF EVERY session file (up to `limit`),
    // so it is blocking I/O and must not run on an async worker.
    let digests =
        match tokio::task::spawn_blocking(move || transcript::session_digests_batch(&all_sessions))
            .await
        {
            Ok(d) => d,
            Err(e) => {
                warn!("transcript_session_digests: digest task failed: {e}");
                Vec::new()
            }
        };

    info!(
        "transcript_session_digests: computed {} digests",
        digests.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Computed {} session digests", digests.len())),
        data: Some(serde_json::to_value(&digests).unwrap_or_default()),
    })
}

/// Detect Claude Code processes running outside this Runner instance.
///
/// Returns a list of PIDs and optional working directories for Claude processes
/// that are NOT managed by this Runner's PTY or session system.
#[tauri::command]
pub async fn transcript_find_external_processes(
    execution: tauri::State<'_, ExecutionCompartment>,
) -> Result<CommandResponse, String> {
    // Collect PIDs managed by this runner
    let managed_pids = execution
        .ai_pid_tracker()
        .lock()
        .map_err(|e| format!("Failed to lock ai_pid_tracker: {e}"))?
        .clone();

    let external = transcript::find_external_claude_processes(&managed_pids);

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Found {} external Claude processes",
            external.len()
        )),
        data: Some(serde_json::to_value(&external).unwrap_or_default()),
    })
}

/// Generate a workflow from arbitrary text context (no task_run_id required).
///
/// This is the standalone generation entrypoint for terminal text selections
/// and transcript imports. Calls the same pipeline as `generate_workflow_from_session`
/// but doesn't need an existing AI session.
#[tauri::command]
pub async fn generate_workflow_standalone(
    storage: tauri::State<'_, StorageCompartment>,
    health: tauri::State<'_, HealthCompartment>,
    description: String,
    inline_context: String,
    include_ui_bridge: Option<bool>,
) -> Result<CommandResponse, String> {
    info!(
        "generate_workflow_standalone: desc_len={}, context_len={}",
        description.len(),
        inline_context.len()
    );

    if inline_context.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No context provided for workflow generation".to_string()),
            data: None,
        });
    }

    // Detect brief-mode: the Specs-page AI path prefixes its inline_context
    // with "Spec Generation Brief (JSON)". Those calls want the brief fed to
    // the Builder verbatim — the transcript-style spec-aware rewrap
    // (`build_spec_aware_context` is a "generate NEW spec files from a plan"
    // envelope) would compete with the brief's "consume EXISTING spec into
    // snapshot_assert steps" instructions and the Builder produces an empty
    // workflow because the two voices contradict. Brief mode also skips the
    // specification-synthesis agent for the same reason.
    let is_brief_mode = inline_context.contains("Spec Generation Brief (JSON)");

    let enriched_context = if is_brief_mode {
        // Pass through unchanged — the brief carries its own instructions and
        // meta_workflow.rs::build_builder_prompt will append the matching
        // "Spec Generation Brief Recognition" rules section.
        inline_context.clone()
    } else {
        // `fetch_existing_specs` is a pre-compartment helper that takes
        // `&AppState`. Use the explicit `app_state()` escape hatch on
        // health to keep the legacy passthrough greppable.
        let existing_specs =
            crate::commands::ai_session::fetch_existing_specs(health.app_state()).await;
        if existing_specs != "No existing specs found" {
            crate::commands::ai_session::build_spec_aware_context(&inline_context, &existing_specs)
        } else {
            format!(
                "The following is a Claude Code conversation transcript or selected text. \
                 Use this context to generate an appropriate workflow:\n\n{}",
                inline_context
            )
        }
    };

    // Build generation request
    let (request_category, request_tags, generate_spec_flag) = if is_brief_mode {
        (
            Some("spec-generated".to_string()),
            Some(vec!["spec".to_string(), "auto-generated".to_string()]),
            Some(false),
        )
    } else {
        (None, None, Some(true))
    };

    let request = crate::workflow_generation::GenerateWorkflowRequest {
        // Stream C: transcript-replay workflows target the runner app by default.
        app_id: crate::spec_api::storage::RUNNER_APP_ID.to_string(),
        description,
        inline_context: Some(enriched_context),
        category: request_category,
        tags: request_tags,
        max_iterations: None,
        provider: None,
        model: None,
        skip_ai_summary: None,
        log_source_selection: None,
        prompt_template: None,
        auto_include_contexts: Some(true),
        context_ids: None,
        max_fix_iterations: Some(3),
        discovery_mode: None,
        include_ui_bridge_instructions: include_ui_bridge,
        reflection_mode: None,
        investigate_codebase: Some(true),
        include_design_guidance: None,
        auto_run: None,
        model_overrides: None,
        generate_specification: generate_spec_flag,
        verification_depth: None,
        discover_ui_bridge_specs: None,
        simple_mode: None,
        pipeline_depth: None,
        tool_tags: None,
        exploration_settings: None,
        target_runner_port: None,
    };

    // Get doctor handle for health monitoring
    let doctor_handle = health.doctor_handle().lock().await.clone();
    let pg_db = storage.pg_db().clone();
    let pg_clone = pg_db.clone();
    // Clone the AppState Arc so the builder's brief-mode port resolution
    // can use the actually-bound runner port (matters for temp runners on
    // 9877+). Without this the synchronous path falls back to
    // get_mcp_api_port() which reads $QONTINUI_PORT / defaults to 9876.
    // `generate_workflow` is a pre-compartment helper; escape-hatch via
    // health.app_state() keeps the legacy passthrough greppable.
    let app_state_for_gen = health.app_state().clone();

    // catch_unwind safety net: converts any latent panic inside the generation
    // pipeline (e.g. a future PG deserialization drift) into a clean structured
    // error response instead of a bare JoinError::Panic bubbling to the frontend.
    let gen_result = tokio::task::spawn_blocking(move || {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            crate::workflow_generation::generate_workflow(
                request,
                doctor_handle.as_ref(),
                Some(&pg_clone),
                None,
                Some(&*app_state_for_gen),
            )
        }));
        match panic_result {
            Ok(pair) => pair,
            Err(payload) => {
                let reason = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                tracing::error!("generate_workflow panicked: {}", reason);
                let response = crate::workflow_generation::GenerateWorkflowResponse {
                    workflow: None,
                    validation_errors: Vec::new(),
                    success: false,
                    error: Some(format!("Workflow generation panicked: {}", reason)),
                    model_used: None,
                    verification_iterations: Vec::new(),
                    hardening_summary: None,
                    discovery_calls: Vec::new(),
                    acceptance_criteria: None,
                    quality_report: None,
                    confidence_score: None,
                    workflow_evaluation: None,
                    exploration_stats: None,
                };
                // Empty artifact placeholder so the save path can skip cleanly.
                let artifact =
                    crate::workflow_generation::pipeline_artifacts::PipelineArtifactBuilder::new(
                        "", None,
                    )
                    .build(0);
                (response, artifact)
            }
        }
    })
    .await;

    // Save pipeline artifact to PG (async, outside spawn_blocking)
    if let Ok((_, ref artifact)) = gen_result {
        if let Err(e) = pg_db.save_generation_artifact(artifact).await {
            tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
        }
    }
    let gen_result = gen_result.map(|(response, _)| response);

    match gen_result {
        Ok(mut response) => {
            // Save workflow to database if generation succeeded
            if response.success {
                if let Some(ref workflow) = response.workflow {
                    let create_req = crate::unified_workflows::CreateUnifiedWorkflowRequest {
                        name: workflow.name.clone(),
                        description: workflow.description.clone(),
                        category: workflow.category.clone(),
                        tags: workflow.tags.clone(),
                        setup_steps: workflow.setup_steps.clone(),
                        verification_steps: workflow.verification_steps.clone(),
                        agentic_steps: workflow.agentic_steps.clone(),
                        completion_steps: workflow.completion_steps.clone(),
                        max_iterations: workflow.max_iterations,
                        timeout_seconds: workflow.timeout_seconds,
                        provider: workflow.provider.clone(),
                        model: workflow.model.clone(),
                        skip_ai_summary: false,
                        log_source_selection: None,
                        context_ids: None,
                        disabled_context_ids: None,
                        auto_include_contexts: Some(workflow.auto_include_contexts),
                        prompt_template: workflow.prompt_template.clone(),
                        log_watch_enabled: Some(workflow.log_watch_enabled),
                        health_check_enabled: Some(workflow.health_check_enabled),
                        health_check_urls: if workflow.health_check_urls.is_empty() {
                            None
                        } else {
                            Some(workflow.health_check_urls.clone())
                        },
                        preflight_check_enabled: Some(workflow.preflight_check_enabled),
                        enable_sweep: Some(workflow.enable_sweep),
                        max_sweep_iterations: Some(workflow.max_sweep_iterations),
                        targeted_error_ids: None,
                        generated_by_task_run_id: None,
                        stages: if workflow.stages.is_empty() {
                            None
                        } else {
                            Some(workflow.stages.clone())
                        },
                        stop_on_failure: Some(workflow.stop_on_failure),
                        constraint_overrides: Some(workflow.constraint_overrides.clone()),
                        approval_gate: Some(workflow.approval_gate),
                        reflection_mode: Some(workflow.reflection_mode),
                        completion_prompts_first: Some(workflow.completion_prompts_first),
                        model_overrides: Some(workflow.model_overrides.clone()),
                        dependency_graph: workflow.dependency_graph.clone(),
                        cost_annotations: workflow.cost_annotations.clone(),
                        quality_report: workflow.quality_report.clone(),
                        acceptance_criteria: workflow.acceptance_criteria.clone(),
                        ai_reviewed: Some(workflow.ai_reviewed),
                        workflow_architecture: workflow.workflow_architecture.clone(),
                        enforce_token_budget: Some(workflow.enforce_token_budget),
                        strict_cwd: workflow.strict_cwd,
                        tool_tags: workflow.tool_tags.clone(),
                        flow_control_json: None,
                        phase_timeouts_json: None,
                        rollback_policy: workflow.rollback_policy.clone(),
                        htn_enabled: workflow.htn_enabled,
                        htn_ui_bridge_url: workflow.htn_ui_bridge_url.clone(),
                        htn_state_machine_path: workflow.htn_state_machine_path.clone(),
                    };

                    match storage.pg_db().create_unified_workflow(&create_req).await {
                        Ok(saved) => {
                            info!(
                                "Saved standalone generated workflow '{}' (id={})",
                                saved.name, saved.id
                            );
                            // Overwrite the pre-save generator UUID with the persisted
                            // DB id so callers can navigate to the actual row without
                            // re-POSTing and creating a duplicate.
                            if let Some(ref mut wf) = response.workflow {
                                wf.id = saved.id.clone();
                            }
                        }
                        Err(e) => {
                            warn!("Failed to save standalone generated workflow: {}", e);
                        }
                    }
                }
            }

            Ok(CommandResponse {
                success: response.success,
                message: response.error.clone().or_else(|| {
                    response
                        .workflow
                        .as_ref()
                        .map(|w| format!("Generated workflow: {}", w.name))
                }),
                data: Some(serde_json::to_value(&response).unwrap_or_default()),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Generation task failed: {}", e)),
            data: None,
        }),
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_transcript")
        .invoke_handler(tauri::generate_handler![
            transcript_list_sessions,
            transcript_read_session,
            transcript_read_user_prompts,
            transcript_get_latest,
            transcript_session_digests,
            transcript_find_external_processes,
            generate_workflow_standalone,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(names: &[&str]) -> Vec<std::path::PathBuf> {
        names.iter().map(std::path::PathBuf::from).collect()
    }

    #[test]
    fn fresh_entry_with_matching_inputs_is_servable() {
        let p = vec!["a".to_string()];
        let d = dirs(&["cfg"]);
        assert!(entry_is_servable(Duration::from_millis(1), &p, &d, &p, &d));
    }

    #[test]
    fn expired_entry_is_not_servable() {
        let p = vec!["a".to_string()];
        let d = dirs(&["cfg"]);
        assert!(!entry_is_servable(
            SCAN_CACHE_TTL + Duration::from_millis(1),
            &p,
            &d,
            &p,
            &d
        ));
    }

    /// The cache key must include BOTH inputs. Serving an `all_projects:false`
    /// caller the workspace-wide entry (or vice-versa) would silently return
    /// the wrong session set.
    #[test]
    fn differing_inputs_are_not_servable() {
        let d = dirs(&["cfg"]);
        assert!(
            !entry_is_servable(
                Duration::ZERO,
                &["a".to_string()],
                &d,
                &["b".to_string()],
                &d
            ),
            "different project paths must not share an entry"
        );
        assert!(
            !entry_is_servable(
                Duration::ZERO,
                &["a".to_string()],
                &dirs(&["cfg1"]),
                &["a".to_string()],
                &dirs(&["cfg2"])
            ),
            "different config dirs must not share an entry"
        );
    }

    /// The regression this module exists for: six terminal tabs polling in one
    /// burst must produce ONE scan, not six.
    ///
    /// Single test rather than three, because the cache and the scan counter
    /// are process-global — separate `#[tokio::test]`s would race each other.
    /// Empty `project_paths` makes the underlying scan a no-op, so this
    /// measures the caching decision and nothing else.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_collapse_to_one_scan() {
        let paths: Vec<String> = Vec::new();
        let key_a = dirs(&["scan-cache-test-a"]);
        let before = scans_performed();

        let mut handles = Vec::new();
        for _ in 0..6 {
            let p = paths.clone();
            let d = key_a.clone();
            handles.push(tokio::spawn(async move {
                collect_all_sessions_cached(&p, &d).await
            }));
        }
        for h in handles {
            h.await.expect("scan task joined");
        }
        assert_eq!(
            scans_performed() - before,
            1,
            "six concurrent callers must collapse to a single scan"
        );

        // A further poll inside the TTL is served from the entry.
        let _ = collect_all_sessions_cached(&paths, &key_a).await;
        assert_eq!(
            scans_performed() - before,
            1,
            "a repeat within the TTL must not rescan"
        );

        // Different inputs get their own scan — the key is honoured.
        let _ = collect_all_sessions_cached(&paths, &dirs(&["scan-cache-test-b"])).await;
        assert_eq!(
            scans_performed() - before,
            2,
            "a different cache key must force its own scan"
        );
    }
}
