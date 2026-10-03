//! The production [`FanoutHost`] and [`FanoutEvents`]: the runner process the
//! members actually run in.
//!
//! `spawn_fanout_member` is an AUTONOMOUS spawn site in
//! `runner_spawn_sites.txt` — it spawns on the admission tick, not on a click —
//! so it passes coord's device drain gate itself (through
//! `authorize_fanout_spawn_with_budget`, which checks the drain first) and
//! branches on the verdict before [`spawn_member_terminal`] reaches the shared
//! spawn seam. The dispatcher's [`FanoutHost::drain_deferral`] pre-check only
//! keeps a drained tick from churning the ledger; this gate is the authority.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tauri::{Emitter, Manager};
use tracing::warn;

use super::dispatcher::{FanoutEvents, FanoutHost, Liveness, MemberSpawnRequest, SpawnOutcome};
use super::model::{ConfigDirPolicy, RunView};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// The Tauri event every fan-out state change is announced on.
pub(crate) const FANOUT_CHANGED_EVENT: &str = "fanout-changed";

/// Queueing budget for the `parallel_fanout` admission. Near zero on purpose:
/// the admission tick IS the retry mechanism, and queueing inside it would hold
/// the dispatcher's book (and every operator route) for the module's 120 s
/// default. `SlotUnavailable` simply leaves the member queued for the next tick.
const ADMISSION_WAIT: Duration = Duration::from_millis(250);

/// The drain origin a fan-out admission carries. A fan-out run is the
/// operator's queue, but its spawns happen on a tick with no operator at the
/// keyboard, so they are autonomous work — the same `orchestration` class the
/// conductor's own worker fan-out uses, and deferred by a drain exactly as it
/// is.
const ORIGIN: crate::coord_drain_state::SpawnOrigin =
    crate::coord_drain_state::SpawnOrigin::Orchestration;

pub(crate) struct TauriFanoutHost {
    pub app: tauri::AppHandle,
}

#[async_trait]
impl FanoutHost for TauriFanoutHost {
    fn drain_deferral(&self) -> Option<String> {
        match crate::coord_drain_state::drain_gate(ORIGIN) {
            crate::coord_drain_state::DrainGate::Allow => None,
            crate::coord_drain_state::DrainGate::Defer { reason, .. } => Some(reason),
        }
    }

    async fn fanout_bound(&self) -> u32 {
        crate::agent_authorization::current_fanout_bound(None).await
    }

    async fn spawn_fanout_member(&self, req: MemberSpawnRequest) -> SpawnOutcome {
        let work_key = crate::coord_drain_state::bounded_work_key(
            "fanout",
            &format!("{}:{}", req.run_id, req.index),
        );
        let slot = match crate::agent_authorization::authorize_fanout_spawn_with_budget(
            None,
            ADMISSION_WAIT,
            crate::agent_authorization::DrainAdmission::work(ORIGIN, work_key),
        )
        .await
        {
            crate::agent_authorization::FanoutAdmission::Admitted { slot, .. } => slot,
            crate::agent_authorization::FanoutAdmission::DeferredByDrain { reason, .. } => {
                return SpawnOutcome::DeferredByDrain { reason };
            }
            crate::agent_authorization::FanoutAdmission::SlotUnavailable {
                bound, waited, ..
            } => {
                return SpawnOutcome::BoundOccupied {
                    detail: format!(
                        "parallel_fanout bound {bound} fully occupied for {}ms",
                        waited.as_millis()
                    ),
                };
            }
            crate::agent_authorization::FanoutAdmission::Refused(decision) => {
                return SpawnOutcome::Refused {
                    reason: decision.refusal().unwrap_or_else(|| {
                        format!("spawn-authorization {}: refused", decision.label())
                    }),
                };
            }
        };
        let outcome = match spawn_member_terminal(&self.app, &req).await {
            Ok(terminal_id) => SpawnOutcome::Spawned { terminal_id },
            Err(reason) => SpawnOutcome::Refused { reason },
        };
        // The slot covers the admission, not the session's lifetime: the run's
        // own cap is what bounds how many members run at once.
        drop(slot);
        outcome
    }

    fn liveness(&self, claude_session_id: &str, terminal_id: Option<&str>) -> Liveness {
        let store = self
            .app
            .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
            .map(|s| s.inner().clone());
        let record = store.as_ref().and_then(|s| s.get(claude_session_id));
        if record.as_ref().is_some_and(|r| r.finished_at.is_some()) {
            return Liveness::Finished;
        }
        let Some(manager) = self
            .app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
            .map(|s| s.inner().clone())
        else {
            return Liveness::Unknown;
        };
        // The lifecycle record follows a session across a restore rebind; the
        // member's own terminal id is the fallback when there is no record.
        let candidates = record
            .map(|r| r.terminal_id)
            .into_iter()
            .chain(terminal_id.map(str::to_string));
        for tid in candidates {
            if manager.get(&tid).is_some_and(|s| s.is_alive()) {
                return Liveness::Live { terminal_id: tid };
            }
        }
        Liveness::Exited
    }
}

/// Spawn one member as a visible terminal whose PTY child is `claude` with the
/// member's prompt as the trailing positional argument — the gate-continuation
/// recipe ([`crate::agent_runtime::build_continuation_claude_command`]), so the
/// prompt is in the session from its first byte: no idle-scrape, no timer.
///
/// Returns the terminal id, or the refusal text (a `resource_guard:critical:`
/// prefix for a resource refusal, the seam's own text otherwise).
async fn spawn_member_terminal(
    app: &tauri::AppHandle,
    req: &MemberSpawnRequest,
) -> Result<String, String> {
    // UNATTENDED spawn — respect the critical floor, and refuse BEFORE the
    // worktree allocation below, which a refusal would otherwise leak (the
    // `terminal_create` early-out, same reasoning). A refusal is not a failure:
    // the member goes back to the queue and the next tick asks again.
    crate::resource_guard::precheck_spawn("fan-out member", false)?;

    let terminal_manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "TerminalManager state not managed".to_string())?;
    let session_registry = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "SessionRegistry state not managed".to_string())?;

    // Members that edit one repo must not share a checkout: the same
    // `working_dir → intent_repo` derivation `terminal_create` uses routes each
    // one through its own isolated worktree when worktree mode is on.
    let intent_repo =
        crate::commands::terminal::effective_intent_repo(None, Some(&req.working_dir));
    let (working_dir, isolated_ctx) = crate::agent_worktree::isolated_edit::acquire_for_terminal(
        intent_repo.as_deref(),
        &req.title,
        Some(req.working_dir.clone()),
        None,
        req.tenant_id,
    )
    .await;
    let working_dir = working_dir.unwrap_or_else(|| req.working_dir.clone());

    // The ABSOLUTE binary: this argv is the PTY child's program (direct exec, no
    // shell), and a bare `claude` resolves to the extensionless identity shim.
    let claude_bin = spawn_blocking_tracked(crate::agent_runtime::resolve_claude_bin)
        .await
        .unwrap_or_else(|_| crate::agent_runtime::claude_bin_path());

    let selected_config_dir = match &req.config_dir_policy {
        ConfigDirPolicy::BestHeadroom => {
            let _ = spawn_blocking_tracked(crate::ai_provider::pick_best_account).await;
            let ai = crate::settings::get_ai_settings();
            crate::ai_provider::get_effective_config_dir(&ai.claude_cli).0
        }
        ConfigDirPolicy::Fixed { config_dir } => Some(config_dir.clone()),
    };
    if selected_config_dir.is_none()
        && !crate::ai_provider::oauth_refresh::default_location_has_valid_credentials()
    {
        let instance = crate::instance::instance_name().unwrap_or_else(|| "primary".to_string());
        return Err(format!(
            "no authenticated Claude account on this runner — run /login (instance={instance})"
        ));
    }
    let launch_cfg = crate::claude_session::launch_spec::LaunchConfig::from_settings(
        selected_config_dir.as_deref(),
    );

    // An isolated worktree is the member's own, so it gets the coord-mcp
    // config and the fleet commands the gate-continuation path writes into
    // its worktree. A shared checkout gets neither — writing them there would
    // clobber the operator's own files — and the briefing asserts no liveness.
    let (add_dir_args, coord_mcp) = match isolated_ctx.as_ref() {
        Some(ctx) => {
            let bound_port = app
                .try_state::<Arc<crate::commands::AppState>>()
                .map(|s| crate::mcp::types::runner_api_port(s.inner()));
            let delivery = crate::coord_mcp::provision_coord_mcp_for_session(
                &working_dir,
                bound_port,
                req.tenant_id,
            );
            crate::fleet_commands::provision_fleet_commands_for_session(&working_dir);
            crate::fleet_skills::provision_fleet_skills_for_session(&working_dir);
            (ctx.claude_add_dir_args(), delivery)
        }
        None => (Vec::new(), crate::coord_mcp::CoordMcpDelivery::Unknown),
    };
    let prompt_carrier = crate::session::spawn_prompt::resolve_system_prompt_carrier(Some(
        crate::terminal::runner_context(crate::terminal::spawn_seam_api_port(), coord_mcp),
    ));
    let policy_delivery = prompt_carrier.as_ref().and_then(|c| c.policy_delivery());
    let argv = crate::agent_runtime::build_continuation_claude_command(
        claude_bin,
        &req.claude_session_id,
        add_dir_args,
        req.prompt.clone(),
        prompt_carrier,
        // Direct exec — no identity shim in the chain to append `--settings`.
        crate::session::claude_hook::direct_spawn_settings_args(),
        &launch_cfg,
    );
    let policy_delivery =
        crate::session::spawn_prompt::delivery_unless_replacement(policy_delivery, &argv);

    let counts: Vec<(String, usize)> = {
        let mut per_page: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for info in terminal_manager.list() {
            *per_page.entry(info.page_id).or_insert(0) += 1;
        }
        per_page.into_iter().collect()
    };
    let target_page = crate::agent_runtime::pick_continuation_page(
        &counts,
        crate::agent_runtime::CONTINUATION_PAGE_ZONE_CEILING,
        || uuid::Uuid::new_v4().to_string(),
    );

    let capture_hint = crate::commands::terminal::SessionCaptureHint {
        config_dir: selected_config_dir,
        working_dir: working_dir.clone(),
        title: req.title.clone(),
        page_id: Some(target_page.clone()),
        // Matches the `--session-id` in the argv → recorded synchronously.
        claude_session_id: Some(req.claude_session_id.clone()),
        zone_index: None,
        // The operator's own work: keep the host git identity, as a tab they
        // opened by hand would.
        inject_agent_git_identity: false,
        gate_identity: None,
        coord_lineage: Some(
            crate::commands::terminal::CoordSessionLineage::for_pinned_session(
                &req.claude_session_id,
            ),
        ),
        policy_delivery,
    };

    crate::claude_session::trust_gate::warm_dial().await;

    crate::commands::terminal::create_tracked_terminal_session_backend(
        &terminal_manager,
        &session_registry,
        app.clone(),
        req.title.clone(),
        working_dir,
        None,
        Some(format!("fanout:{}", req.run_id)),
        intent_repo,
        Some(argv),
        isolated_ctx,
        capture_hint,
        Some(target_page),
        // UNATTENDED — nobody is at a dialog to answer "Start anyway".
        false,
        // The run's tenant, admitted once at create.
        req.tenant_id,
    )
    .map(|(terminal_id, _coord_session)| terminal_id)
}

/// Announces every changed run on the [`FANOUT_CHANGED_EVENT`] Tauri event,
/// payload = the run's full [`RunView`].
pub(crate) struct TauriFanoutEvents {
    pub app: tauri::AppHandle,
}

impl FanoutEvents for TauriFanoutEvents {
    fn changed(&self, run: &RunView) {
        if let Err(e) = self.app.emit(FANOUT_CHANGED_EVENT, run) {
            warn!(run_id = %run.id, error = %e, "fanout: failed to emit {FANOUT_CHANGED_EVENT}");
        }
    }
}
