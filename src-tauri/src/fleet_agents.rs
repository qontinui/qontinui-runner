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
//! ## The `.md` files in `fleet_agents/` are the CANONICAL sources
//!
//! As in the two sibling modules: ordinary files in this public repository,
//! reviewed through a normal pull request, with git history as the tamper
//! record. Because they ship to every fleet device they must stay free of any
//! one operator's absolute paths — see
//! [`tests::bundled_agent_defs_have_no_operator_local_paths`].

use std::path::Path;

use include_dir::{include_dir, Dir};

use crate::capability_manifest::{self, ProvisionReport, SkipReason};

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
/// overwrites existing files (idempotent).
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
pub(crate) fn provision_fleet_agents_into(dst_dir: &Path) -> std::io::Result<ProvisionReport> {
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
        // Per-file rather than `?`: one unwritable definition must not cost the
        // session the other four, and it must be NAMED rather than aborting the
        // pass at whatever point it happened to reach.
        match std::fs::write(dst_dir.join(name), file.contents()) {
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

        let report = provision_fleet_agents_into(&dst).expect("provision");
        assert_eq!(
            report.written,
            embedded_agent_count(),
            "should write every embedded definition"
        );
        assert_eq!(report.expected, embedded_agent_count());
        assert!(report.skipped.is_empty(), "nothing should be skipped here");
        assert!(report.is_complete(), "a full pass must not read as degraded");
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

        let first = provision_fleet_agents_into(&dst).expect("first");
        let victim = dst.join("code-reviewer.md");
        std::fs::write(&victim, b"CLOBBERED").expect("clobber");

        let second = provision_fleet_agents_into(&dst).expect("second");
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
