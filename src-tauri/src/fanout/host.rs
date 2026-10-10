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

use std::sync::atomic::{AtomicI64, Ordering};
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
        // The registry resolves per DEVICE, not per tenant: there is no tenant
        // argument to scope it by, so this is this runner's bound for every
        // run it holds, whatever tenant each run was admitted under.
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
        let store = match self
            .app
            .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
        {
            // An unmanaged store is not "no record": nothing can be concluded.
            None => StoreRead::Unreadable,
            Some(s) => match s.get_checked(claude_session_id) {
                Err(e) => {
                    // Asked once per admitted member per tick: a store that
                    // stays poisoned would otherwise warn every 5 s per member.
                    if take_warn_slot(
                        &LAST_UNREADABLE_WARN,
                        chrono::Utc::now().timestamp(),
                        UNREADABLE_WARN_INTERVAL_SECS,
                    ) {
                        warn!(session = %claude_session_id, error = %e,
                            "fanout: lifecycle store unreadable — liveness UNKNOWN \
                             (repeats suppressed for {UNREADABLE_WARN_INTERVAL_SECS}s)");
                    } else {
                        tracing::debug!(session = %claude_session_id, error = %e,
                            "fanout: lifecycle store unreadable — liveness UNKNOWN");
                    }
                    StoreRead::Unreadable
                }
                Ok(None) => StoreRead::Absent,
                Ok(Some(r)) => StoreRead::Record(RecordFacts {
                    finished: r.finished_at.is_some(),
                    open: r.state == "open" && r.closed_at.is_none(),
                    terminal_id: r.terminal_id,
                }),
            },
        };
        let manager = self
            .app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
            .map(|s| s.inner().clone());
        // Terminals pinned to the session at create — found even when the
        // spawner died before the lifecycle record or the member's terminal id
        // was written.
        let pinned = manager
            .as_ref()
            .map(|m| m.terminal_ids_pinned_to(claude_session_id))
            .unwrap_or_default();
        classify_liveness(
            store,
            manager
                .as_ref()
                .map(|m| move |tid: &str| -> Option<bool> { m.get(tid).map(|s| s.is_alive()) }),
            terminal_id,
            &pinned,
        )
    }
}

/// At most one "lifecycle store unreadable" warning per this many seconds.
const UNREADABLE_WARN_INTERVAL_SECS: i64 = 300;

/// When the last "lifecycle store unreadable" warning was logged (unix secs).
static LAST_UNREADABLE_WARN: AtomicI64 = AtomicI64::new(i64::MIN);

/// Whether a rate-limited warning may be logged at `now` (unix secs), claiming
/// the slot if so. Racing callers: exactly one wins a given slot.
fn take_warn_slot(last: &AtomicI64, now: i64, interval_secs: i64) -> bool {
    let prev = last.load(Ordering::Relaxed);
    if prev != i64::MIN && now.saturating_sub(prev) < interval_secs {
        return false;
    }
    last.compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// What the lifecycle store says about one session.
pub(crate) enum StoreRead {
    /// Poisoned lock, or no store in this process: nothing can be concluded.
    Unreadable,
    /// The store answered and holds no record of the session.
    Absent,
    Record(RecordFacts),
}

/// The lifecycle-record fields liveness reads.
pub(crate) struct RecordFacts {
    pub finished: bool,
    /// `state == "open"` and not closed.
    pub open: bool,
    /// The terminal the record says hosts the session (follows a restore rebind).
    pub terminal_id: String,
}

/// Liveness from the lifecycle record and the terminal manager. Pure, so every
/// arm is tested without a Tauri app.
///
/// `probe(tid)` is `Some(alive)` for a terminal the manager holds and `None`
/// for one it does not; `probe` itself is `None` when no manager is managed.
///
/// `pinned` is every terminal the manager holds whose pinned harness id is the
/// session's — the one place a `claude` whose spawner died after
/// `TerminalManager::create` but before its lifecycle record (or the member's
/// terminal id) was written can still be found. Without it such a member reads
/// [`Liveness::Unrecorded`] and is released `spawn_unconfirmed` while it runs.
///
/// The arm that matters is the open record with NO terminal answering for it:
/// a session mid-respawn, or one session restore has not rebound yet, reads
/// exactly like that for a while — so it is [`Liveness::Unconfirmed`] (the
/// dispatcher holds the slot for a grace window), never an exit. A terminal
/// that IS present and dead, or a closed record, is a real exit.
pub(crate) fn classify_liveness<P: Fn(&str) -> Option<bool>>(
    store: StoreRead,
    probe: Option<P>,
    member_terminal: Option<&str>,
    pinned: &[String],
) -> Liveness {
    let record = match store {
        StoreRead::Unreadable => return Liveness::Unknown,
        StoreRead::Absent => None,
        StoreRead::Record(r) => Some(r),
    };
    if record.as_ref().is_some_and(|r| r.finished) {
        return Liveness::Finished;
    }
    let Some(probe) = probe else {
        return Liveness::Unknown;
    };
    // The lifecycle record follows a session across a restore rebind; the
    // member's own terminal id is the fallback.
    let candidates = record
        .as_ref()
        .map(|r| r.terminal_id.clone())
        .into_iter()
        .chain(member_terminal.map(str::to_string))
        .chain(pinned.iter().cloned());
    let mut a_terminal_answered = false;
    for tid in candidates {
        match probe(&tid) {
            Some(true) => return Liveness::Live { terminal_id: tid },
            Some(false) => a_terminal_answered = true,
            None => {}
        }
    }
    match record {
        Some(r) if r.open && !a_terminal_answered => Liveness::Unconfirmed,
        Some(_) => Liveness::Exited,
        None if a_terminal_answered => Liveness::Exited,
        None => Liveness::Unrecorded,
    }
}

/// Spawn one member as a visible terminal whose PTY child is `claude` with the
/// member's prompt as the trailing positional argument — the gate-continuation
/// recipe ([`crate::agent_runtime::build_continuation_claude_command`]), so the
/// prompt is in the session from its first byte: no idle-scrape, no timer.
///
/// Every check that can refuse — the resource floor, the managed state, the
/// account — runs BEFORE `acquire_for_terminal` allocates a worktree. A refusal
/// the spawn seam itself returns after the allocation (the trust gate, the
/// seam's own resource gate) hands the allocation back
/// ([`crate::agent_worktree::isolated_edit::AllocationHandback`]): the
/// dispatcher retries a refused member, and each retry would otherwise mint one
/// more orphaned worktree and ledger row.
///
/// Returns the terminal id, or the refusal text (a `resource_guard:critical:`
/// prefix for a resource refusal, the seam's own text otherwise).
async fn spawn_member_terminal(
    app: &tauri::AppHandle,
    req: &MemberSpawnRequest,
) -> Result<String, String> {
    // UNATTENDED spawn — respect the critical floor. A refusal is not a
    // failure: the member goes back to the queue and is retried with backoff.
    crate::resource_guard::precheck_spawn("fan-out member", false)?;

    let terminal_manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "TerminalManager state not managed".to_string())?;
    let session_registry = app
        .try_state::<Arc<crate::session::SessionRegistry>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "SessionRegistry state not managed".to_string())?;

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
    crate::claude_session::trust_gate::warm_dial().await;

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
    let handback = isolated_ctx.as_ref().map(|ctx| ctx.handback());
    // An isolated worktree is the member's own, so it gets the fleet commands
    // and skills before the served-corpus probe below reads its `.claude/` —
    // provision first, then measure, as the looping spawn does. A shared
    // checkout gets neither: writing them there would clobber the operator's
    // own files.
    if isolated_ctx.is_some() {
        crate::fleet_commands::provision_fleet_commands_for_session(&working_dir);
        crate::fleet_skills::provision_fleet_skills_for_session(&working_dir);
    }
    // The served-corpus line of the member's briefing, measured against the
    // directory the member actually runs in, on the blocking pool (the
    // gate/condition/looping spawns' convention).
    let served = crate::served_corpus::probe_async(working_dir.as_str()).await;

    let result = launch_member(
        app,
        req,
        LaunchInputs {
            terminal_manager,
            session_registry,
            claude_bin,
            selected_config_dir,
            launch_cfg,
            intent_repo,
            working_dir,
            isolated_ctx,
            served,
        },
    );
    let err = match result {
        Ok(terminal_id) => return Ok(terminal_id),
        Err(err) => err,
    };
    match (on_launch_failure(&err), handback) {
        (LaunchFailure::HandBack, Some(handback)) => {
            // Refused before a PTY child existed, so nothing ever ran in the
            // worktree: hand it back now.
            handback.abandon(err.message()).await;
        }
        (LaunchFailure::LeaveForReclaim, Some(_)) => {
            warn!(run_id = %req.run_id, index = req.index, error = %err,
                "fanout: a member's child was spawned and then killed — its worktree is \
                 left for the reclaim engine, not handed back");
        }
        (_, None) => {}
    }
    Err(err.into())
}

/// What a failed launch does with the allocation acquired for it.
#[derive(Debug, PartialEq, Eq)]
enum LaunchFailure {
    /// No child ever existed: remove the worktree and retire the allocation.
    HandBack,
    /// A child existed (and was killed): something may have run in the
    /// worktree, so it is the reclaim engine's to judge, never removed here.
    LeaveForReclaim,
}

fn on_launch_failure(err: &crate::terminal::CreateError) -> LaunchFailure {
    if err.child_spawned() {
        LaunchFailure::LeaveForReclaim
    } else {
        LaunchFailure::HandBack
    }
}

/// Everything [`launch_member`] needs that [`spawn_member_terminal`] resolved.
struct LaunchInputs {
    terminal_manager: Arc<crate::terminal::TerminalManager>,
    session_registry: Arc<crate::session::SessionRegistry>,
    claude_bin: String,
    selected_config_dir: Option<String>,
    launch_cfg: crate::claude_session::launch_spec::LaunchConfig,
    intent_repo: Option<String>,
    working_dir: String,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    served: crate::served_corpus::ServedCorpus,
}

/// Build the member's argv and hand it to the shared spawn seam. Its only
/// error is the seam's, typed by whether a PTY child ever existed: a refusal
/// made before the child, or a child spawned and then killed because its
/// session could not be built ([`crate::terminal::CreateError`]). The seam
/// never returns an error with a child still running.
fn launch_member(
    app: &tauri::AppHandle,
    req: &MemberSpawnRequest,
    inputs: LaunchInputs,
) -> Result<String, crate::terminal::CreateError> {
    let LaunchInputs {
        terminal_manager,
        session_registry,
        claude_bin,
        selected_config_dir,
        launch_cfg,
        intent_repo,
        working_dir,
        isolated_ctx,
        served,
    } = inputs;

    // An isolated worktree also gets the coord-mcp config the gate-continuation
    // path writes into its worktree (its fleet commands and skills were
    // provisioned before the served-corpus probe). A shared checkout gets
    // none, and the briefing asserts no liveness.
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
            (ctx.claude_add_dir_args(), delivery)
        }
        None => (Vec::new(), crate::coord_mcp::CoordMcpDelivery::Unknown),
    };
    let prompt_carrier = crate::session::spawn_prompt::resolve_system_prompt_carrier(Some(
        crate::terminal::runner_context(crate::terminal::spawn_seam_api_port(), coord_mcp, &served),
    ));
    let policy_delivery = prompt_carrier.as_ref().and_then(|c| c.policy_delivery());
    // `claude --name` for the member, from the same title its tab carries —
    // the gate/condition/looping spawns' convention. It becomes the commit
    // `Session-Name` trailer and the tab's immutable `spawnName`; `None` (a
    // title that sanitises to nothing) leaves the argv without `--name`.
    let spawn_name = crate::claude_session::launch_spec::sanitize_session_name(&req.title);
    let argv = crate::agent_runtime::build_continuation_claude_command(
        claude_bin,
        &req.claude_session_id,
        spawn_name.as_deref(),
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
        spawn_name,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn record(open: bool, finished: bool, tid: &str) -> StoreRead {
        StoreRead::Record(RecordFacts {
            finished,
            open,
            terminal_id: tid.to_string(),
        })
    }

    /// `terminals`: tid → alive. A tid absent from the map is not held.
    fn classify(store: StoreRead, terminals: &[(&str, bool)], member: Option<&str>) -> Liveness {
        let map: HashMap<String, bool> = terminals
            .iter()
            .map(|(t, a)| ((*t).to_string(), *a))
            .collect();
        classify_liveness(store, Some(|tid: &str| map.get(tid).copied()), member, &[])
    }

    #[test]
    fn fanout_an_open_record_with_no_terminal_answering_is_unconfirmed_not_exited() {
        // Mid-respawn, or session restore has not rebound it yet.
        assert_eq!(
            classify(record(true, false, "t-old"), &[], Some("t-old")),
            Liveness::Unconfirmed
        );
    }

    #[test]
    fn fanout_a_poisoned_or_absent_store_is_unknown() {
        assert_eq!(
            classify(StoreRead::Unreadable, &[("t1", false)], Some("t1")),
            Liveness::Unknown
        );
        let no_manager: Option<fn(&str) -> Option<bool>> = None;
        assert_eq!(
            classify_liveness(record(true, false, "t1"), no_manager, Some("t1"), &[]),
            Liveness::Unknown
        );
    }

    #[test]
    fn fanout_real_exits_and_live_rebinds_are_still_read() {
        // A rebound session is live under the record's NEW terminal.
        assert_eq!(
            classify(
                record(true, false, "t-new"),
                &[("t-new", true)],
                Some("t-old")
            ),
            Liveness::Live {
                terminal_id: "t-new".to_string()
            }
        );
        // A terminal present and dead is an exit, open record or not.
        assert_eq!(
            classify(record(true, false, "t1"), &[("t1", false)], Some("t1")),
            Liveness::Exited
        );
        // A closed record is an exit.
        assert_eq!(
            classify(record(false, false, "t1"), &[], Some("t1")),
            Liveness::Exited
        );
        assert_eq!(
            classify(record(true, true, "t1"), &[("t1", true)], Some("t1")),
            Liveness::Finished
        );
    }

    /// Only a refusal made before any child existed hands the worktree back;
    /// a child that was spawned (then killed) leaves it for reclaim.
    #[test]
    fn fanout_handback_runs_only_for_a_refusal_before_the_child() {
        use crate::terminal::CreateError;
        assert_eq!(
            on_launch_failure(&CreateError::BeforeChild("trust gate".into())),
            LaunchFailure::HandBack
        );
        assert_eq!(
            on_launch_failure(&CreateError::ChildKilled("no writer".into())),
            LaunchFailure::LeaveForReclaim
        );
    }

    /// A persistently unreadable lifecycle store warns once per interval, not
    /// once per member per tick.
    #[test]
    fn fanout_unreadable_store_warning_is_rate_limited() {
        let last = AtomicI64::new(i64::MIN);
        assert!(take_warn_slot(&last, 1_000, 300), "the first one is logged");
        assert!(!take_warn_slot(&last, 1_005, 300));
        assert!(!take_warn_slot(&last, 1_299, 300));
        assert!(
            take_warn_slot(&last, 1_300, 300),
            "the next interval logs again"
        );
    }

    #[test]
    fn fanout_no_record_and_no_terminal_is_unrecorded() {
        assert_eq!(classify(StoreRead::Absent, &[], None), Liveness::Unrecorded);
        assert_eq!(
            classify(StoreRead::Absent, &[("t1", false)], Some("t1")),
            Liveness::Exited
        );
    }

    /// A spawner that died after `TerminalManager::create` registered the
    /// child but before the lifecycle record (or the member's terminal id)
    /// existed: the terminal pinned to the session is found, so the member
    /// reads live — never `Unrecorded`, which would release a running claude
    /// `spawn_unconfirmed`.
    #[test]
    fn fanout_a_terminal_pinned_to_the_session_is_found_without_a_record() {
        let map: HashMap<String, bool> = [("t-pinned".to_string(), true)].into();
        let probe = Some(|tid: &str| map.get(tid).copied());
        assert_eq!(
            classify_liveness(StoreRead::Absent, probe, None, &["t-pinned".to_string()]),
            Liveness::Live {
                terminal_id: "t-pinned".to_string()
            }
        );
        // Pinned and already dead: it ran, so an exit — not "never started".
        let dead: HashMap<String, bool> = [("t-pinned".to_string(), false)].into();
        assert_eq!(
            classify_liveness(
                StoreRead::Absent,
                Some(|tid: &str| dead.get(tid).copied()),
                None,
                &["t-pinned".to_string()]
            ),
            Liveness::Exited
        );
        // Nothing pinned and nothing recorded is still unrecorded.
        assert_eq!(
            classify_liveness(
                StoreRead::Absent,
                Some(|tid: &str| map.get(tid).copied()),
                None,
                &[]
            ),
            Liveness::Unrecorded
        );
    }
}
