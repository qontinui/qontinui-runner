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
use std::path::Path;

use tracing::{info, warn};

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
    provision_session_assets_from_root(
        crate::agent_runtime::qontinui_root_dir().as_deref(),
        workdir,
    );
}

/// Core of [`provision_session_assets`] with the workspace root passed in, so a
/// test can drive the canonical-source arm without mutating process env.
///
/// **The canonical-source arm.** A cwd at the workspace root sees
/// `<root>/.claude` as a symlink into `qontinui-claude-config/.claude` — the
/// canonical sources every bundled asset was copied from. Writing there, even
/// file by file behind the tracked-file probe, would overwrite them whenever the
/// probe fails soft (a timeout reads as "nothing tracked"). So the verdict is
/// taken ONCE for the whole `.claude` tree, and when it holds no asset kind is
/// written: the session already has every asset, as the source files. Each kind
/// still gets a ledger row saying so.
fn provision_session_assets_from_root(root: Option<&Path>, workdir: &str) {
    let claude_dir = Path::new(workdir).join(".claude");
    if let Some(why) = root.and_then(|r| {
        crate::provision_guard::destination_is_source(
            &claude_dir,
            &r.join("qontinui-claude-config").join(".claude"),
        )
    }) {
        info!("session_assets: not provisioning {workdir} — {why}");
        for capability in CANONICAL_SOURCE_ROWS {
            let dst = claude_dir.display().to_string();
            let mut report =
                ProvisionReport::new(capability, 0, capability_manifest::Rung::OperatorCheckout)
                    .with_destination(dst.clone());
            report.skip(
                dst,
                capability_manifest::SkipReason::CanonicalSource(why.clone()),
            );
            capability_manifest::record_provision(workdir, report);
        }
        return;
    }
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

/// The ledger rows the canonical-source arm records — one per capability the
/// ordinary arm would have reported.
const CANONICAL_SOURCE_ROWS: [&str; 4] = [
    "fleet_agents",
    "agent_definitions",
    "fleet_commands",
    "fleet_skills",
];

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

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(dir).unwrap().display().to_string();
                    out.push((rel, std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// The workspace-root shape: `<cwd>/.claude` links into the checkout's
    /// `.claude`. Nothing under the source tree may change — agents, commands
    /// and skills alike, tracked or not — and every asset kind is reported as
    /// standing down on the canonical source.
    #[cfg(unix)]
    #[test]
    fn a_cwd_whose_claude_tree_is_the_checkout_source_is_left_untouched() {
        let _store = capability_manifest::store_lock();
        let root = tempfile::tempdir().unwrap();
        let claude = root.path().join("qontinui-claude-config").join(".claude");
        for (rel, body) in [
            ("agents/code-reviewer.md", "# canonical reviewer"),
            ("commands/vet-plan.md", "# canonical vet-plan"),
            ("skills/coord-revive/SKILL.md", "# canonical skill"),
            ("commands/draft.md", "# an untracked draft"),
        ] {
            let path = claude.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let before = listing(&claude);

        let cwd = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&claude, cwd.path().join(".claude")).unwrap();
        let cwd_s = cwd.path().to_string_lossy().into_owned();

        provision_session_assets_from_root(Some(root.path()), &cwd_s);

        assert_eq!(
            listing(&claude),
            before,
            "the canonical source tree must be byte-identical"
        );
        let ledger = capability_manifest::session_provision_ledger(&cwd_s).expect("recorded");
        assert_eq!(
            ledger
                .reports
                .iter()
                .map(|r| (r.capability, r.skipped[0].reason.wire()))
                .collect::<Vec<_>>(),
            CANONICAL_SOURCE_ROWS
                .iter()
                .map(|c| (*c, "canonical_source"))
                .collect::<Vec<_>>()
        );
    }
}
