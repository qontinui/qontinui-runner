//! The one entry point that provisions the runner-bundled assets into a spawned
//! session's cwd (plan
//! `2026-09-02-gate-continuation-sessions-get-no-subagent-definitions`).
//!
//! A session receives three asset kinds, each with its own provisioner:
//!
//! | Kind | Destination | Provisioner |
//! |---|---|---|
//! | subagent definitions | `.claude/agents/*.md` | [`crate::agent_runtime::provision_agent_definitions`] (embedded floor + checkout overlay) |
//! | fleet slash commands | `.claude/commands/*.md` | [`crate::fleet_commands::provision_fleet_commands_for_session`] |
//! | fleet skills | `.claude/skills/<name>/` | [`crate::fleet_skills::provision_fleet_skills_for_session`] |
//!
//! Every spawn path used to call these inline, and the sequences drifted: only
//! the agent-spawn path wrote agent definitions, so a gate-continuation session
//! could not spawn `code-reviewer`, and the headless continuation arm wrote no
//! fleet asset at all. Routing every site through [`provision_session_assets`]
//! (or its async twin [`provision_session_assets_off_runtime`]) makes "which
//! assets does a session get?" a question with one answer, and
//! `session_asset_sites` (test-only) fails on any provisioner call made anywhere
//! else, and on any spawn path that stops calling this module.
//!
//! **Fail-soft, exactly as each provisioner already is.** Nothing here returns
//! an error or panics: a provision that cannot write degrades into a ledger row
//! ([`crate::capability_manifest::record_provision`]) and a log line, and the
//! spawn proceeds. All three skip a destination the enclosing git repository
//! tracks ([`crate::provision_guard`]), so a cwd whose `.claude/` is a checkout
//! keeps its own content.

use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use tracing::warn;

use crate::capability_manifest::{self, ProvisionReport};

/// Provision agent definitions, fleet commands and fleet skills into `workdir`.
///
/// Synchronous and BLOCKING: each provisioner writes files and runs one bounded
/// `git ls-files` probe. From an async spawn path call
/// [`provision_session_assets_off_runtime`] instead, so the probes never occupy
/// a tokio worker. What was written is read back through
/// [`capability_manifest::session_provision_ledger`] or
/// `GET /capability-manifest/sessions`.
pub(crate) fn provision_session_assets(workdir: &str) {
    match crate::agent_runtime::provision_agent_definitions(workdir) {
        Ok(report) => capability_manifest::record_provision(workdir, report),
        Err(e) => {
            warn!("session_assets: agent-def provisioning into {workdir} errored (continuing spawn): {e:#}");
            // Still a ROW: an errored pass that leaves no record is exactly the
            // invisible degradation the ledger exists to end.
            let mut report = ProvisionReport::new(
                "agent_definitions",
                0,
                capability_manifest::Rung::Unresolved,
            )
            .with_destination(workdir.to_string());
            report.skip(
                workdir.to_string(),
                capability_manifest::SkipReason::WriteFailed(format!("{e:#}")),
            );
            capability_manifest::record_provision(workdir, report);
        }
    }
    crate::fleet_commands::provision_fleet_commands_for_session(workdir);
    crate::fleet_skills::provision_fleet_skills_for_session(workdir);
}

/// [`provision_session_assets`] on the blocking pool, awaited — for async spawn
/// paths. A panic or cancellation on the pool (a `JoinError`) is logged and
/// swallowed: provisioning never aborts a spawn, and a session missing some
/// assets is the same state a failed write already produces.
pub(crate) async fn provision_session_assets_off_runtime(workdir: &str) {
    let owned = workdir.to_string();
    if let Err(e) = spawn_blocking_tracked(move || provision_session_assets(&owned)).await {
        warn!(
            "session_assets: provisioning task for {workdir} did not complete \
             (continuing spawn without some session assets): {e}"
        );
    }
}
