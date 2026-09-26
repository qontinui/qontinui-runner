//! The one entry point that provisions the runner-bundled assets into a spawned
//! session's cwd (plan
//! `2026-09-02-gate-continuation-sessions-get-no-subagent-definitions`).
//!
//! A session receives three asset kinds, each with its own provisioner:
//!
//! | Kind | Destination | Provisioner |
//! |---|---|---|
//! | subagent definitions | `.claude/agents/*.md` | [`crate::agent_runtime::provision_agent_definitions_from_root`] (embedded floor + checkout overlay) |
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
//! tracks, and never write through a symlink ([`crate::provision_guard`]), so a
//! cwd whose `.claude/` is a checkout — or links into one — keeps its content.

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
        &Registries::resolve(),
    );
}

/// The command and skill registries one provisioning pass writes from (or, on
/// the canonical-source arm, only observes).
///
/// Resolved ONCE per spawn by [`provision_session_assets`] and passed down,
/// rather than resolved inside the provisioners, for two reasons: every arm —
/// including the canonical-source one, which writes nothing — records which
/// resolution arm answered from the same value; and a test hands in
/// [`Registries::embedded`] instead, because real resolution reads the
/// credential store (with an OS-keychain fallback), makes a live fetch, and
/// stores or clears the on-disk override cache under the platform config dir.
pub(crate) struct Registries {
    pub(crate) commands: crate::agent_commands::AgentCommandRegistry,
    pub(crate) skills: crate::agent_skills::AgentSkillRegistry,
}

impl Registries {
    /// Fresh fetch → disk cache → embedded defaults, for each registry. Not
    /// free (a network fetch with a disk-cache fallback): call it only from a
    /// blocking context.
    fn resolve() -> Self {
        Self {
            commands: crate::agent_commands::resolve_registry(),
            skills: crate::agent_skills::resolve_registry(),
        }
    }

    /// The embedded defaults alone — what a device with no account resolves
    /// to — with no credential read, fetch or cache write.
    #[cfg(test)]
    pub(crate) fn embedded() -> Self {
        Self {
            commands: crate::agent_commands::AgentCommandRegistry::new(),
            skills: crate::agent_skills::AgentSkillRegistry::new(),
        }
    }
}

/// Core of [`provision_session_assets`] with the workspace root and the
/// registries passed in — each resolved ONCE per spawn by the wrapper — so a
/// test can drive every arm without mutating process env or touching the
/// network, the credential store or the override cache.
///
/// **The canonical-source arm.** A cwd at the workspace root sees
/// `<root>/.claude` as a symlink into `qontinui-claude-config/.claude` — the
/// canonical sources every bundled asset was copied from. Writing there, even
/// file by file behind the tracked-file probe, would overwrite them whenever the
/// probe fails soft (a timeout reads as "nothing tracked"). So the verdict is
/// taken ONCE for the whole `.claude` tree, and when it holds no asset kind is
/// written: the session already has every asset, as the source files. Each kind
/// still gets a ledger row saying so, and both registries still record which
/// arm resolved them.
///
/// The verdict holds on either of two tests: the cwd's `.claude` resolves into
/// the canonical `.claude` (a path compare, following symlinks), or the cwd is a
/// work tree of the canonical REPOSITORY — a linked worktree of
/// `qontinui-claude-config` has a real `.claude/` at another path, which the
/// path compare misses ([`crate::provision_guard::same_repository`]). The
/// identity test walks up to the cwd's NEAREST `.git`, so it deliberately covers
/// a cwd nested anywhere inside such a work tree, not only its top level.
fn provision_session_assets_from_root(root: Option<&Path>, workdir: &str, registries: &Registries) {
    let claude_dir = Path::new(workdir).join(".claude");
    if let Some(why) = root.and_then(|r| {
        let checkout = r.join("qontinui-claude-config");
        crate::provision_guard::destination_is_source(&claude_dir, &checkout.join(".claude"))
            .or_else(|| crate::provision_guard::same_repository(Path::new(workdir), &checkout))
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
        // Which arm of each registry answered is a fact about resolution, not
        // provisioning, so it is recorded even though nothing is written; the
        // two provisioners are its only other recorders.
        crate::fleet_commands::observe_commands_registry(&registries.commands);
        crate::fleet_skills::observe_skills_registry(&registries.skills);
        return;
    }
    match crate::agent_runtime::provision_agent_definitions_from_root(root, workdir) {
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
    crate::fleet_commands::provision_fleet_commands_for_session(workdir, &registries.commands);
    crate::fleet_skills::provision_fleet_skills_for_session(workdir, &registries.skills);
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
        capability_manifest::reset_provision_store();
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

        provision_session_assets_from_root(Some(root.path()), &cwd_s, &Registries::embedded());

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
        assert_registry_observations_recorded();
    }

    /// Standing down on the canonical source skips both provisioners, but not
    /// the facts they report about resolution: WHICH arm of each registry
    /// answered is still a capability-manifest row, so a box whose spawns all
    /// land on the canonical source does not read those rows as unknown.
    fn assert_registry_observations_recorded() {
        for capability in ["agent_commands_registry", "agent_skills_registry"] {
            assert!(
                capability_manifest::latest_observation(capability).is_some(),
                "{capability} must be observed on the canonical-source arm"
            );
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} should succeed");
    }

    /// `git init` a repo at `repo` with one committed `.claude/agents` file,
    /// then `git worktree add` it at `worktree`.
    fn seed_repo_with_worktree(repo: &Path, worktree: &Path) {
        let agents = repo.join(".claude").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        crate::provision_guard::test_support::git_init(repo);
        std::fs::write(agents.join("code-reviewer.md"), "# committed reviewer").unwrap();
        git(repo, &["add", "--", ".claude"]);
        git(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "-m",
                "seed",
            ],
        );
        git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &worktree.to_string_lossy(),
            ],
        );
    }

    /// The negative twin of the worktree test: a worktree of an UNRELATED
    /// repository, with the canonical checkout present under the same root, is
    /// provisioned as usual. A repository-identity check that answered "same"
    /// for everything would stand every session down; this goes red on it.
    #[test]
    fn a_worktree_of_an_unrelated_repository_is_provisioned() {
        let _store = capability_manifest::store_lock();
        capability_manifest::reset_provision_store();
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        seed_repo_with_worktree(
            &root.path().join("qontinui-claude-config"),
            &elsewhere.path().join("config-worktree"),
        );
        let worktree = elsewhere.path().join("unrelated-worktree");
        seed_repo_with_worktree(&elsewhere.path().join("unrelated"), &worktree);
        let cwd_s = worktree.to_string_lossy().into_owned();

        provision_session_assets_from_root(Some(root.path()), &cwd_s, &Registries::embedded());

        let ledger = capability_manifest::session_provision_ledger(&cwd_s).expect("recorded");
        assert!(
            ledger
                .reports
                .iter()
                .flat_map(|r| &r.skipped)
                .all(|skip| skip.reason.wire() != "canonical_source"),
            "an unrelated repository is not the canonical source"
        );
        let commands = worktree.join(".claude").join("commands");
        assert!(
            std::fs::read_dir(&commands).is_ok_and(|mut d| d.next().is_some()),
            "fleet commands were written into {}",
            commands.display()
        );
    }

    /// A linked worktree OF the canonical checkout — the shape of an
    /// `agent-worktrees/<id>/qontinui-claude-config`. Its `.claude/` is a real
    /// directory (no symlink to stand it down) at a path that is not
    /// `<root>/qontinui-claude-config/.claude`, so the path compare misses; the
    /// only thing left would be the fail-soft tracked-file probe, which WRITES
    /// every path it does not see tracked. Repository identity must stand every
    /// kind down instead: nothing written, a canonical-source row per kind.
    #[test]
    fn a_worktree_of_the_canonical_checkout_is_left_untouched() {
        let _store = capability_manifest::store_lock();
        capability_manifest::reset_provision_store();
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let worktree = elsewhere.path().join("qontinui-claude-config");
        seed_repo_with_worktree(&root.path().join("qontinui-claude-config"), &worktree);
        let before = listing(&worktree.join(".claude"));
        let cwd_s = worktree.to_string_lossy().into_owned();

        provision_session_assets_from_root(Some(root.path()), &cwd_s, &Registries::embedded());

        assert_eq!(
            listing(&worktree.join(".claude")),
            before,
            "the worktree's .claude tree must be byte-identical"
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
        assert_registry_observations_recorded();
    }

    /// No workspace root resolved (a published install), and the cwd's
    /// `.claude` is a symlink: the root-based check cannot run, so the
    /// never-through-a-symlink rule alone must stand every kind down and leave
    /// the target tree byte-identical.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_claude_tree_with_no_root_is_left_untouched() {
        let _store = capability_manifest::store_lock();
        let target = tempfile::tempdir().unwrap();
        for (rel, body) in [
            ("agents/code-reviewer.md", "# someone's reviewer"),
            ("commands/vet-plan.md", "# someone's vet-plan"),
        ] {
            let path = target.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let before = listing(target.path());

        let cwd = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), cwd.path().join(".claude")).unwrap();
        let cwd_s = cwd.path().to_string_lossy().into_owned();

        provision_session_assets_from_root(None, &cwd_s, &Registries::embedded());

        assert_eq!(
            listing(target.path()),
            before,
            "nothing written through the link"
        );
        let ledger = capability_manifest::session_provision_ledger(&cwd_s).expect("recorded");
        let rows: Vec<(&str, &str)> = ledger
            .reports
            .iter()
            .map(|r| (r.capability, r.skipped[0].reason.wire()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("fleet_agents", "symlinked"),
                ("agent_definitions", "symlinked"),
                ("fleet_commands", "symlinked"),
                ("fleet_skills", "symlinked"),
            ]
        );
    }
}
