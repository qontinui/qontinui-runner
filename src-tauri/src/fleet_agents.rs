//! The named-subagent definitions this binary ships.
//!
//! Third sibling of [`crate::fleet_commands`] (`.claude/commands/*.md`) and
//! [`crate::fleet_skills`] (`.claude/skills/<name>/SKILL.md`). This one covers
//! `.claude/agents/*.md` — the definitions `claude` reads to resolve a named
//! subagent, e.g. the `merge-specialist` an auto-spawned review prompt invokes,
//! or the `code-reviewer` the fleet's pre-PR-review policy names.
//!
//! ## Why this exists
//!
//! `agent_runtime::provision_agent_definitions` COPIES these from a
//! `qontinui-claude-config` checkout, and its own docstring has named the fix
//! ever since:
//!
//! > *The fleet-portability follow-up is to BUNDLE these defs into the runner
//! > binary (`include_str!`) so non-operator devices without a
//! > `qontinui-claude-config` checkout still get them; this copy-from-checkout
//! > path unblocks the current operator fleet.*
//!
//! Without a checkout the copy is a no-op that `warn!`s and returns `Ok`, so a
//! spawned agent silently has no subagents: `claude` cannot resolve the named
//! subagent, the review never runs, and coord eventually ages the PR out as
//! `specialist_timeout`. That is a failure with no error at the point of cause.
//!
//! ## Checkout still WINS — this is a floor, not a replacement
//!
//! [`provision_agent_definitions_from_root`] now writes these embedded defaults
//! FIRST and then overlays any checkout copies on top. An operator editing
//! `qontinui-claude-config/.claude/agents/*.md` keeps the live-edit workflow
//! they have today, byte for byte; a device with no checkout gets the embedded
//! set instead of nothing. The change is strictly additive — no configuration
//! that worked before resolves differently now.
//!
//! ## The `.md` files in `fleet_agents/` are a RENDER, not the source of truth
//!
//! They are ordinary files in this public repository, reviewed through a normal
//! pull request, with git history as the tamper record. But the CONTENT
//! originates in `qontinui-claude-config/.claude/agents/*.md` — the copy the
//! checkout overlay above writes on top of these, and the one humans edit.
//! Edit a definition THERE and copy it forward byte for byte; an edit made
//! directly here is drift the moment it lands. Same direction as
//! [`crate::fleet_skills`], whose header was corrected the same way.
//!
//! This header used to call these files "the CANONICAL sources", and nothing
//! carried config edits across: from the bundle's creation (`0245c2e96`,
//! 2026-08-28) to its first full re-sync, `merge-specialist.md` drifted ~700
//! lines and kept telling runner-provisioned specialists about a coord executor
//! that no longer acts on their decision (coord finding `491c906f`). Unlike the
//! skills bundle, no publish-cycle or CI parity check covers this directory
//! yet, so a re-sync is still a hand carry.
//!
//! Because they ship to every fleet device they must stay free of any one
//! operator's absolute paths — see
//! [`tests::bundled_agent_defs_have_no_operator_local_paths`].

use std::path::Path;

use include_dir::{include_dir, Dir};

use crate::capability_manifest::{self, ProvisionReport, SkipReason};
use crate::provision_guard::TrackedPaths;

/// The embedded subagent definitions. A flat directory of `*.md`, matching what
/// `claude` expects under `.claude/agents/`.
///
/// A [`Dir`] rather than [`crate::fleet_commands::FLEET_COMMANDS`]' explicit
/// `(name, body)` roster: these are all one shape with no per-entry wiring, so
/// **adding an agent is adding a `.md` file here and nothing else**. Same
/// reasoning as [`crate::fleet_skills`], and the same crate already backs
/// `spec_api::storage::EMBEDDED_PAGES`.
static FLEET_AGENTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/fleet_agents");

/// Write every embedded subagent definition into `dst_dir`, returning a
/// [`ProvisionReport`] describing what landed. Creates `dst_dir` if absent;
/// overwrites existing files (idempotent) — EXCEPT a destination that already
/// exists and that `tracked` says the enclosing git repository tracks, which is
/// skipped and reported as [`SkipReason::GitTracked`] (see
/// [`crate::provision_guard`]). The caller probes `tracked` once for `dst_dir`
/// and shares it with the checkout overlay, which writes the same directory.
///
/// The guard matters because every session spawn path provisions agent
/// definitions now, including cwds that are checkouts: `qontinui-claude-config`
/// TRACKS every `.claude/agents/*.md`, and an unguarded floor write there would
/// replace the canonical sources with this binary's older snapshot and leave
/// the tree dirty.
///
/// Only `*.md` at the top level is written — the same filter the checkout copy
/// applies, so the two layers cannot disagree about what counts as a definition.
///
/// **The report changes no behaviour.** This function was fail-soft from its
/// caller's side already (`agent_runtime` catches the `Err`, warns, and
/// continues to the checkout overlay); it used to return a bare `usize`, which
/// could say how many defs landed but never which ones did not, or why. A
/// definition that never lands is the silent failure this module exists to
/// remove — `claude` cannot resolve the named subagent, the review never runs,
/// and coord ages the PR out as `specialist_timeout` with no error at the point
/// of cause — so "how many" was never the interesting half.
pub(crate) fn provision_fleet_agents_into(
    dst_dir: &Path,
    tracked: &TrackedPaths,
) -> std::io::Result<ProvisionReport> {
    std::fs::create_dir_all(dst_dir)?;
    let mut out = ProvisionReport::new(
        "fleet_agents",
        embedded_agent_count(),
        capability_manifest::Rung::Embedded,
    )
    .with_destination(dst_dir.display().to_string());
    for file in FLEET_AGENTS.files() {
        if file.path().extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = file.path().file_name() else {
            continue;
        };
        let unit = name.to_string_lossy().into_owned();
        let dst = dst_dir.join(name);
        // Never through a symlink: a per-file link into a canonical checkout is
        // invisible to `tracked`, which asks the SESSION repo.
        if let Some(why) = crate::provision_guard::symlink_below(dst_dir, &dst) {
            out.skip(unit, SkipReason::Symlinked(why));
            continue;
        }
        if tracked.should_skip(&dst, Path::new(name)) {
            tracing::info!(
                "fleet_agents: skipping {} — it is tracked by the enclosing git \
                 repository, and overwriting it would replace that repo's own content",
                dst.display()
            );
            out.skip(unit, SkipReason::GitTracked);
            continue;
        }
        // Per-file rather than `?`: one unwritable definition must not cost the
        // session the other four, and it must be NAMED rather than aborting the
        // pass at whatever point it happened to reach.
        match std::fs::write(&dst, file.contents()) {
            Ok(()) => out.record_written(),
            Err(e) => out.skip(unit, SkipReason::WriteFailed(e.to_string())),
        }
    }
    if out.written == 0 {
        out.set_rung(capability_manifest::Rung::Unresolved);
    }
    Ok(out)
}

/// Number of embedded subagent definitions, for log lines that want to say how
/// many defaults were available independent of how many the checkout overlaid.
pub(crate) fn embedded_agent_count() -> usize {
    FLEET_AGENTS
        .files()
        .filter(|f| f.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_every_embedded_agent_def() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let dst = tmp.path().join(".claude").join("agents");

        let report =
            provision_fleet_agents_into(&dst, &TrackedPaths::probe(&dst)).expect("provision");
        assert_eq!(
            report.written,
            embedded_agent_count(),
            "should write every embedded definition"
        );
        assert_eq!(report.expected, embedded_agent_count());
        assert!(report.skipped.is_empty(), "nothing should be skipped here");
        assert!(
            report.is_complete(),
            "a full pass must not read as degraded"
        );
        assert_eq!(report.rung, crate::capability_manifest::Rung::Embedded);
        assert!(report.written > 0, "the bundle should not be empty");

        for file in FLEET_AGENTS.files() {
            let name = file.path().file_name().expect("named");
            let path = dst.join(name);
            assert!(path.exists(), "{} should exist", path.display());
            let on_disk = std::fs::read(&path).expect("read provisioned def");
            assert_eq!(
                on_disk,
                file.contents(),
                "{} should be byte-identical to the embedded copy",
                path.display()
            );
        }
    }

    #[test]
    fn provision_is_idempotent_overwrite() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let dst = tmp.path().join(".claude").join("agents");

        let first = provision_fleet_agents_into(&dst, &TrackedPaths::probe(&dst)).expect("first");
        let victim = dst.join("code-reviewer.md");
        std::fs::write(&victim, b"CLOBBERED").expect("clobber");

        let second = provision_fleet_agents_into(&dst, &TrackedPaths::probe(&dst)).expect("second");
        assert_eq!(
            (first.written, first.skipped.len()),
            (second.written, second.skipped.len()),
            "both passes write the same count"
        );
        assert_ne!(
            std::fs::read(&victim).expect("read restored"),
            b"CLOBBERED",
            "re-provisioning must overwrite a modified def, not leave it"
        );
    }

    /// A destination TRACKED by the enclosing repository keeps the repo's bytes
    /// and is reported skipped with its reason; an untracked sibling in the same
    /// repo is still written. `qontinui-claude-config` tracks every
    /// `.claude/agents/*.md`, so this is the shipped behaviour for a session
    /// spawned there, not a corner case.
    #[test]
    fn a_git_tracked_agent_def_is_skipped_and_an_untracked_sibling_written() {
        use crate::provision_guard::test_support::{git_add, git_init};

        let tmp = tempfile::tempdir().expect("create tempdir");
        let dst = tmp.path().join(".claude").join("agents");
        std::fs::create_dir_all(&dst).unwrap();
        git_init(tmp.path());

        let tracked = dst.join("code-reviewer.md");
        std::fs::write(&tracked, b"# the repo's own reviewer\n").unwrap();
        git_add(tmp.path(), &tracked);
        // Present but deliberately NOT `git add`ed.
        let untracked = dst.join("repo-auditor.md");
        std::fs::write(&untracked, b"stale, untracked\n").unwrap();

        let report =
            provision_fleet_agents_into(&dst, &TrackedPaths::probe(&dst)).expect("provision");

        assert_eq!(
            std::fs::read(&tracked).unwrap(),
            b"# the repo's own reviewer\n",
            "a tracked definition must be left byte-identical"
        );
        assert_eq!(report.skipped.len(), 1, "only the tracked file is skipped");
        assert_eq!(report.skipped[0].unit, "code-reviewer.md");
        assert_eq!(report.skipped[0].reason, SkipReason::GitTracked);
        assert!(report.is_degraded(), "a skipped unit reads as degraded");
        assert_eq!(report.written, embedded_agent_count() - 1);
        assert_eq!(
            std::fs::read(&untracked).unwrap(),
            FLEET_AGENTS
                .get_file("repo-auditor.md")
                .expect("repo-auditor.md is bundled")
                .contents(),
            "an untracked sibling is overwritten with the embedded copy"
        );
    }

    #[test]
    fn every_embedded_agent_def_declares_a_name() {
        // `claude` resolves a subagent by the `name:` in the definition's YAML
        // frontmatter, NOT by filename. A def whose frontmatter is missing or
        // unnamed is provisioned but unresolvable — the same silent-no-subagent
        // failure this module exists to remove, just moved one step later.
        let mut checked = 0usize;
        for file in FLEET_AGENTS.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            let path = file.path().display();
            assert!(
                text.starts_with("---"),
                "bundled agent def {path} has no YAML frontmatter — `claude` cannot \
                 resolve it; add one in src-tauri/src/fleet_agents/"
            );
            let front = text.split("---").nth(1).unwrap_or_default();
            assert!(
                front.lines().any(|l| l.trim_start().starts_with("name:")),
                "bundled agent def {path} declares no `name:` in its frontmatter — \
                 `claude` resolves a subagent by that field, not by filename, so this \
                 def ships unresolvable"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no agent defs scanned — either the bundle is empty or the include_dir! \
             root went stale"
        );
    }

    #[test]
    fn bundled_agent_defs_have_no_operator_local_paths() {
        // These bodies ship to every fleet device, so a path rooted on one
        // operator's machine is a dead pointer everywhere else. Mirrors
        // `fleet_skills::tests::bundled_skills_have_no_operator_local_paths`.
        const FORBIDDEN: &[&str] = &[
            "D:/qontinui-root",
            "D:\\qontinui-root",
            "C:/Users/",
            "/home/",
        ];
        let mut checked = 0usize;
        for file in FLEET_AGENTS.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            for pat in FORBIDDEN {
                assert!(
                    !text.contains(pat),
                    "bundled agent def {} contains operator-local path {pat:?} — it ships \
                     to every fleet device, where that path does not exist",
                    file.path().display()
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "no agent defs scanned — guard went stale");
    }

    #[test]
    fn the_policy_named_reviewer_is_bundled() {
        // `code-review-invocation-path` requires the `code-reviewer` subagent for
        // every pre-PR review and forbids `/code-review` (operator-only). If this
        // def is not in the bundle, that policy is unsatisfiable on any device
        // without a claude-config checkout — which is the case this module fixes.
        assert!(
            FLEET_AGENTS.get_file("code-reviewer.md").is_some(),
            "code-reviewer.md is not bundled — the fleet's pre-PR-review policy names \
             this subagent specifically, so dropping it from the bundle silently makes \
             that policy unsatisfiable on a checkout-less device"
        );
    }
}
