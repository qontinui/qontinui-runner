//! The agent **skills** this binary ships, and their provisioning into a
//! spawned session's working directory.
//!
//! `claude` discovers project skills from `<cwd>/.claude/skills/<name>/SKILL.md`
//! — the same cwd-rooted rule as `<cwd>/.claude/commands/*.md`, confirmed
//! empirically 2026-08-22. A gate-continuation session is spawned with a fresh
//! worktree as its cwd, so the fleet's own skills (`/coord-revive`, `/policy`,
//! `/gate`, …) are unresolvable there unless something writes them in. This
//! module is that something, and it is the exact sibling of
//! [`crate::fleet_commands`]: an embedded default bundle plus a fail-soft
//! per-session provisioner, with [`crate::agent_skills`] resolving
//! `fresh fetch → disk cache → embedded default` before anything is written.
//!
//! ## The embedded bundle is empty until Phase 6
//!
//! [`FLEET_SKILLS`] ships as an empty slice today, and that is the plan's
//! phasing rather than an oversight: `2026-08-20-fleet-served-agent-skills`
//! Phase 5 seeds the corpus into the account store and Phase 6 moves those
//! bodies into this binary as `include_str!` defaults. Until then the floor of
//! the resolution chain is "no skills", which is byte-identically the state
//! every device is in today — so this module can only add skills to a session,
//! never remove them. The layering, provisioning and refusal rules are all live
//! and tested now; only the default bundle is pending.
//!
//! Nothing in this module or its consumers may assume the bundle's size, in
//! either direction — the same rule [`crate::fleet_commands`] states for the
//! commands.
//!
//! ## Refusing to write is a first-class outcome
//!
//! Writing untracked files into a worktree's `.claude/` makes
//! `git status --porcelain` report `?? .claude/`, and coord's reclaim gate G1
//! ("never remove a dirty worktree") is inviolable — that exact mechanism
//! blinded the reaper across ~240 of 255 coord worktrees until `.claude/` was
//! gitignored (coord `#1181`, schemas `#105`, 2026-07-23). Worse, one repo in
//! this workspace has `.claude/` as **tracked source**: provisioning into a
//! worktree of `qontinui-claude-config` would overwrite the fleet's canonical
//! command and skill sources with account-fetched text, and a run whose cwd is
//! the workspace root reaches the same files through the `.claude` symlink
//! there.
//!
//! So [`claude_dir_write_refusal`] resolves the target first and this module
//! **declines to provision** — loudly, and before spending the fetch budget —
//! when `.claude` is a symlink, when it holds tracked files, or when it is not
//! gitignored. See that function for the full predicate.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::agent_skills::{AgentSkillRegistry, EmbeddedSkill};

/// The embedded default skills, as name + `(relative path, text)` bundle.
///
/// Empty until Phase 6 — see the module docs. Add a skill by adding a
/// directory of `.md`/`.sh` files under `src-tauri/src/fleet_skills/` and one
/// [`EmbeddedSkill`] entry here whose files are `include_str!`ed from it, the
/// way [`crate::fleet_commands::FLEET_COMMANDS`] does. Every entry must satisfy
/// the same rules a fetched unit does; [`tests::embedded_skills_are_provisionable`]
/// enforces that.
pub(crate) const FLEET_SKILLS: &[EmbeddedSkill] = &[];

/// The mode provisioned files are given on Unix: owner-writable, world
/// readable, and **no executable bit anywhere**.
///
/// Scripts in this corpus are invoked as `bash <path>/<script>.sh`, which needs
/// no `+x`, and Windows has no exec bit at all. Keeping the bit off is what
/// stops "account-supplied text written to disk" from becoming
/// "account-supplied program registered with the OS".
pub(crate) const PROVISIONED_FILE_MODE: u32 = 0o644;

/// Provision the resolved agent skills into `<workdir>/.claude/skills/<name>/`
/// so a `claude` session spawned with `workdir` as its cwd can resolve them as
/// PROJECT-scoped skills — even on a device with no `~/.claude/skills` and no
/// `qontinui-claude-config` checkout.
///
/// Fail-soft, mirroring [`crate::fleet_commands::provision_fleet_commands_for_session`]:
/// any IO error is logged via `tracing::warn!` and swallowed — a provisioning
/// failure must never abort an otherwise-launchable spawn. Resolution is
/// fail-soft too: a failed fetch, a rejected credential, a malformed unit, or a
/// broken cache each degrade one step and warn, never propagate. Idempotent:
/// existing files are overwritten.
pub(crate) fn provision_agent_skills_for_session(workdir: &str) {
    let root = Path::new(workdir);
    // Before the fetch, not after: a target we will refuse to write is not
    // worth a network budget on the spawn path.
    if let Some(why) = claude_dir_write_refusal(root) {
        warn!(
            "fleet_skills: declining to provision agent skills into {} — {why}. \
             (Continuing spawn; the session resolves whatever skills its cwd already \
             has.)",
            root.join(".claude").display()
        );
        return;
    }

    let registry = crate::agent_skills::resolve_registry();
    let claude_dir = root.join(".claude");
    match provision_agent_skills_into(&claude_dir, &registry) {
        Ok(written) => {
            info!(
                "fleet_skills: provisioned {} agent skill(s) / {written} file(s) into {} \
                 ({} account skill(s), {} embedded default(s) available)",
                registry.all().len(),
                claude_dir.join("skills").display(),
                registry.override_count(),
                registry.builtin_count(),
            );
        }
        Err(e) => {
            warn!(
                "fleet_skills: failed to provision agent skills into {} \
                 (continuing spawn; the fleet skills may not resolve): {e}",
                claude_dir.join("skills").display()
            );
        }
    }
}

/// Why `<workdir>/.claude/` must not be written, or `None` when it is safe.
///
/// Four refusals, in the order they are cheapest to establish:
///
/// 1. **`.claude` is a symlink** (or, on Windows, a junction — `std` reports
///    both as symlinks). The workspace root's `.claude` links into
///    `qontinui-claude-config/.claude/`, so a run whose cwd is the root would
///    write account-fetched text over tracked fleet source without any repo of
///    that name being the cwd.
/// 2. **`.claude` exists and is not a directory.** `create_dir_all` would fail
///    anyway; refusing here says why.
/// 3. **`.claude/` holds tracked files.** Then it is source, not a scratch
///    area — `qontinui-claude-config` carries 124 tracked files there
///    (measured 2026-08-24).
/// 4. **`.claude/` is not gitignored.** Then whatever is written shows up as
///    `?? .claude/` and pins the worktree out of reclaim forever (gate G1).
///
/// A target that is **not in a git repository at all** is allowed: there is no
/// index to dirty and no reaper to blind. That is established from the
/// filesystem (an ancestor holding a `.git` entry) rather than from `git`
/// itself, so a device without the binary still provisions into a plain
/// directory. But once a `.git` IS found, `git` is the only thing that can
/// answer the last two questions, and **an unanswerable question is a
/// refusal** — fail closed, never "probably fine".
pub(crate) fn claude_dir_write_refusal(workdir: &Path) -> Option<String> {
    let claude = workdir.join(".claude");

    if let Ok(meta) = std::fs::symlink_metadata(&claude) {
        if meta.file_type().is_symlink() {
            return Some(
                "`.claude` is a symlink or junction, so writing here would land in whatever \
                 tree it points at (the workspace root's link into \
                 qontinui-claude-config/.claude/ is exactly this case)"
                    .to_string(),
            );
        }
        if !meta.is_dir() {
            return Some("`.claude` exists and is not a directory".to_string());
        }
    }

    if !is_inside_git_repo(workdir) {
        // No index to dirty and no reaper to blind.
        return None;
    }

    let tracked = match git_stdout(workdir, &["ls-files", "--", ".claude"]) {
        Some(out) => out,
        None => {
            return Some(
                "`.claude/` sits in a git repository but `git ls-files` could not be run, so \
                 whether it is tracked is UNKNOWN — refusing rather than guessing"
                    .to_string(),
            )
        }
    };
    let tracked_count = tracked.lines().filter(|l| !l.trim().is_empty()).count();
    if tracked_count > 0 {
        return Some(format!(
            "`.claude/` is TRACKED SOURCE here ({tracked_count} tracked file(s)) — provisioning \
             would overwrite committed files with account-fetched text"
        ));
    }

    // `check-ignore -q`: 0 = ignored, 1 = not ignored, anything else = fatal.
    let probed = crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(workdir)
        .args(["check-ignore", "-q", "--", ".claude/skills/"])
        .output();
    match probed.as_ref().map(|o| o.status.code()) {
        Ok(Some(0)) => None,
        Ok(Some(1)) => Some(
            "`.claude/` is not gitignored in this repo, so anything written here reports as \
             `?? .claude/` and pins the worktree out of reclaim (gate G1 — never remove a \
             dirty worktree)"
                .to_string(),
        ),
        other => Some(format!(
            "could not establish whether `.claude/` is gitignored (git check-ignore: {other:?}) \
             — refusing rather than guessing"
        )),
    }
}

/// Whether `workdir` or one of its ancestors carries a `.git` entry — a
/// directory in a primary checkout, a file in a linked worktree; either counts.
///
/// Deliberately a filesystem probe rather than `git rev-parse`: it is the one
/// question that must still be answerable on a device with no `git` binary, and
/// answering it wrong in the permissive direction is what the whole refusal
/// exists to prevent.
fn is_inside_git_repo(workdir: &Path) -> bool {
    let mut cursor = Some(workdir);
    while let Some(dir) = cursor {
        if dir.join(".git").exists() {
            return true;
        }
        cursor = dir.parent();
    }
    false
}

/// `git -C <workdir> <args>` stdout on success. `None` when git could not be
/// run or exited non-zero — which every caller treats as a refusal, not as an
/// empty answer.
fn git_stdout(workdir: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.arg("-C").arg(workdir);
    cmd.args(args);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Core of [`provision_agent_skills_for_session`]: write every resolved skill
/// under `<claude_dir>/skills/<name>/`, returning the number of FILES written.
///
/// Split out so a unit test can drive it against a tempdir, mirroring how
/// `provision_fleet_commands_into` factored out its own core.
///
/// Overwrite-idempotent, and deliberately **not** a mirror: a file that exists
/// on disk but is absent from the resolved bundle is left alone. Deleting a
/// subtree of a session's `.claude/skills/` on the strength of a remote list is
/// a much larger hazard than a stale sibling file, and the resolution chain
/// already replaces a skill's WHOLE bundle rather than merging into it.
///
/// Every relative path is re-validated here even though
/// [`crate::agent_skills::validate_override`] already did: this function is
/// `pub(crate)` and takes any registry, so the traversal refusal has to hold at
/// the layer that actually joins the path. A skill with any bad path is skipped
/// entirely rather than partially written.
fn provision_agent_skills_into(
    claude_dir: &Path,
    registry: &AgentSkillRegistry,
) -> std::io::Result<usize> {
    use qontinui_types::agent_text_units::{
        validate_agent_text_unit_file_path, validate_agent_text_unit_name, AgentTextUnitKind,
    };

    let target = AgentTextUnitKind::skill()
        .provisioning_target()
        .expect("the `skill` kind has a provisioning target");

    let mut written = 0usize;
    for skill in registry.all() {
        if let Err(e) = validate_agent_text_unit_name(&skill.name) {
            warn!(
                "fleet_skills: refusing to provision skill {:?} — {e}",
                skill.name
            );
            continue;
        }
        if let Some(bad) = skill
            .files
            .keys()
            .find(|p| validate_agent_text_unit_file_path(p).is_err())
        {
            warn!(
                "fleet_skills: refusing to provision skill {:?} — file path {bad:?} is not a \
                 safe relative path",
                skill.name
            );
            continue;
        }

        for (rel_path, text) in &skill.files {
            let dst = claude_dir.join(target.relative_path(skill.dir_name(), rel_path));
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_file_no_exec(&dst, text)?;
            written += 1;
        }
    }
    Ok(written)
}

/// Write `text` to `path` with no executable bit — see [`PROVISIONED_FILE_MODE`].
///
/// On Windows there is no exec bit to clear, and the mode is inert; the
/// property is asserted where it is expressible.
fn write_file_no_exec(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PROVISIONED_FILE_MODE))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_skills::tests::{bundle, skill_unit, TEST_BUNDLE};
    use std::collections::BTreeMap;

    fn test_registry() -> AgentSkillRegistry {
        AgentSkillRegistry::from_embedded(TEST_BUNDLE)
    }

    // -- the embedded bundle -------------------------------------------------

    /// Whatever Phase 6 embeds must satisfy every rule a fetched unit does —
    /// an embedded default that the resolver would reject is a default nobody
    /// can ever receive. Vacuous while the bundle is empty, and deliberately
    /// written so it stops being vacuous the moment it is filled.
    #[test]
    fn embedded_skills_are_provisionable() {
        for skill in FLEET_SKILLS {
            let files = bundle(skill.files);
            let unit = skill_unit(skill.name, files);
            crate::agent_skills::validate_override(&unit).unwrap_or_else(|e| {
                panic!("embedded skill {:?} is not provisionable: {e}", skill.name)
            });
        }
    }

    // -- provisioning --------------------------------------------------------

    #[test]
    fn provisions_every_embedded_skill_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");
        let registry = test_registry();

        let written = provision_agent_skills_into(&claude, &registry).expect("provision");
        let expected: usize = TEST_BUNDLE.iter().map(|s| s.files.len()).sum();
        assert_eq!(written, expected);

        for skill in TEST_BUNDLE {
            for (rel, text) in skill.files {
                let path = claude.join("skills").join(skill.name).join(rel);
                assert!(path.exists(), "{} should exist", path.display());
                assert_eq!(
                    &std::fs::read_to_string(&path).expect("read"),
                    text,
                    "{}/{rel} must be written byte-identically",
                    skill.name
                );
            }
        }
    }

    #[test]
    fn provision_is_idempotent_overwrite() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");
        let registry = test_registry();
        let expected: usize = TEST_BUNDLE.iter().map(|s| s.files.len()).sum();

        assert_eq!(
            provision_agent_skills_into(&claude, &registry).unwrap(),
            expected
        );
        assert_eq!(
            provision_agent_skills_into(&claude, &registry).unwrap(),
            expected
        );
    }

    /// An account skill lands INSTEAD of the same-named default, and its whole
    /// bundle replaces the default's.
    #[test]
    fn override_is_provisioned_in_place_of_the_default() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");

        let mut registry = test_registry();
        assert_eq!(
            registry.set_overrides(vec![skill_unit(
                "coord-revive",
                bundle(&[("SKILL.md", "# my own coord-revive\n")]),
            )]),
            1
        );

        provision_agent_skills_into(&claude, &registry).expect("provision");
        let dir = claude.join("skills").join("coord-revive");
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
            "# my own coord-revive\n"
        );
    }

    /// A skill bundle carrying a subdirectory lands as a subdirectory.
    #[test]
    fn nested_relative_paths_land_under_the_skill_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");

        let mut registry = AgentSkillRegistry::from_embedded(&[]);
        registry.set_overrides(vec![skill_unit(
            "pr-status",
            bundle(&[
                ("SKILL.md", "# pr-status\n"),
                ("reference/verdicts.md", "# verdicts\n"),
            ]),
        )]);
        assert_eq!(provision_agent_skills_into(&claude, &registry).unwrap(), 2);
        assert!(claude
            .join("skills")
            .join("pr-status")
            .join("reference")
            .join("verdicts.md")
            .exists());
    }

    /// **Falsification gate, at the layer that joins the path.** A registry
    /// carrying a traversal path — built directly, bypassing
    /// `validate_override` — must write nothing at all, and in particular
    /// nothing outside the skill's own directory.
    #[test]
    fn the_provisioner_refuses_a_traversal_path_it_is_handed_directly() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");

        for bad in [
            "../../ESCAPED.md",
            "../ESCAPED.md",
            "..\\..\\ESCAPED.md",
            "a/../../../ESCAPED.md",
            "/etc/passwd",
            "C:/ESCAPED.md",
        ] {
            let mut registry = AgentSkillRegistry::from_embedded(&[]);
            registry.set_unvalidated_overrides(vec![crate::agent_skills::ResolvedSkill {
                name: "evil".to_string(),
                files: bundle(&[("SKILL.md", "# evil\n"), (bad, "pwned\n")]),
                source: crate::agent_skills::SkillSource::Account,
            }]);

            assert_eq!(
                provision_agent_skills_into(&claude, &registry).unwrap(),
                0,
                "{bad:?}: a skill with a traversal path must be skipped ENTIRELY, \
                 not partially written"
            );
            // Not a spot check on one guessed location: nothing named
            // ESCAPED.md may exist ANYWHERE under the tempdir, whichever
            // direction the traversal took.
            let mut found = Vec::new();
            all_files_under(tmp.path(), &mut found);
            assert!(
                !found.iter().any(|p| p.ends_with("ESCAPED.md")),
                "{bad:?}: a file escaped the skill directory: {found:?}"
            );
            assert!(
                !claude.join("skills").join("evil").join("SKILL.md").exists(),
                "{bad:?}: the good half of a bad bundle must not land either"
            );
        }
    }

    /// Every file under `dir`, recursively — the sentinel sweep above.
    fn all_files_under(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                all_files_under(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// A skill whose NAME is a traversal is skipped too.
    #[test]
    fn the_provisioner_refuses_a_traversal_name_it_is_handed_directly() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");
        let mut registry = AgentSkillRegistry::from_embedded(&[]);
        registry.set_unvalidated_overrides(vec![crate::agent_skills::ResolvedSkill {
            name: "../../evil".to_string(),
            files: bundle(&[("SKILL.md", "# evil\n")]),
            source: crate::agent_skills::SkillSource::Account,
        }]);
        assert_eq!(provision_agent_skills_into(&claude, &registry).unwrap(), 0);
        assert!(!tmp.path().join("evil").exists());
    }

    // -- the no-executable-bit property --------------------------------------

    /// The mode provisioned files are given carries no executable bit for any
    /// of owner, group or other. Expressible — and therefore asserted — on
    /// every platform, including the Windows boxes this fleet runs on where
    /// the end-to-end assertion below is compiled out.
    #[test]
    fn the_provisioned_file_mode_has_no_executable_bit() {
        assert_eq!(
            PROVISIONED_FILE_MODE & 0o111,
            0,
            "provisioned files must never be executable: a `.sh` in this corpus is run \
             as `bash <path>`, and an exec bit would turn account-supplied text into an \
             account-supplied program"
        );
        assert_eq!(PROVISIONED_FILE_MODE, 0o644);
    }

    /// End to end on Unix: nothing written by the provisioner is executable,
    /// script or not.
    #[cfg(unix)]
    #[test]
    fn provisioned_files_are_not_executable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("create tempdir");
        let claude = tmp.path().join(".claude");
        let mut registry = AgentSkillRegistry::from_embedded(&[]);
        registry.set_overrides(vec![skill_unit(
            "coord-revive",
            bundle(&[
                ("SKILL.md", "# coord-revive\n"),
                ("coord-revive.sh", "#!/usr/bin/env bash\necho hi\n"),
            ]),
        )]);
        provision_agent_skills_into(&claude, &registry).expect("provision");

        for rel in ["SKILL.md", "coord-revive.sh"] {
            let path = claude.join("skills").join("coord-revive").join(rel);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o111,
                0,
                "{rel} must not be executable (mode {mode:o})"
            );
        }
    }

    // -- the `.claude/` write refusal ----------------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let out = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "--initial-branch=main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "test"]);
    }

    /// A plain directory that is not in a repository is provisionable: there is
    /// no index to dirty and no reaper to blind.
    #[test]
    fn a_non_repo_directory_is_provisionable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(claude_dir_write_refusal(tmp.path()), None);
    }

    /// The normal case: a repo that gitignores `.claude/` and tracks nothing
    /// under it. This is qontinui-web, qontinui-runner and qontinui-schemas.
    #[test]
    fn a_repo_that_ignores_dot_claude_is_provisionable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        init_repo(root);
        std::fs::write(root.join(".gitignore"), ".claude/\n").unwrap();

        assert_eq!(claude_dir_write_refusal(root), None);

        // And the end-to-end path actually writes.
        let mut registry = AgentSkillRegistry::from_embedded(&[]);
        registry.set_overrides(vec![skill_unit(
            "preflight",
            bundle(&[("SKILL.md", "# preflight\n")]),
        )]);
        provision_agent_skills_into(&root.join(".claude"), &registry).expect("provision");
        assert!(root.join(".claude/skills/preflight/SKILL.md").exists());
    }

    /// **The sharp case.** `qontinui-claude-config` does not ignore `.claude/`
    /// and tracks 124 files under it (measured 2026-08-24), and Phase 6 edits
    /// that repo — so a worktree of it is an expected cwd. Provisioning there
    /// would overwrite tracked fleet source with account-fetched text.
    #[test]
    fn a_repo_that_tracks_dot_claude_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        init_repo(root);
        std::fs::create_dir_all(root.join(".claude/skills/coord-revive")).unwrap();
        std::fs::write(
            root.join(".claude/skills/coord-revive/SKILL.md"),
            "# canonical fleet source\n",
        )
        .unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "track .claude"]);

        let why = claude_dir_write_refusal(root).expect("must refuse a tracked .claude/");
        assert!(why.contains("TRACKED SOURCE"), "{why}");

        // The refusal is what the session-facing entrypoint acts on: the
        // tracked file is still exactly as committed afterwards.
        provision_agent_skills_for_session(&root.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(root.join(".claude/skills/coord-revive/SKILL.md")).unwrap(),
            "# canonical fleet source\n"
        );
    }

    /// A repo that tracks nothing under `.claude/` but does not ignore it
    /// either: writing there reports `?? .claude/` and pins the worktree out of
    /// reclaim forever.
    #[test]
    fn a_repo_that_does_not_ignore_dot_claude_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        init_repo(root);
        // Ignoring a CHILD of `.claude/` is not ignoring `.claude/` — this is
        // qontinui-claude-config's actual .gitignore shape.
        std::fs::write(root.join(".gitignore"), ".claude/logs/\n").unwrap();

        let why = claude_dir_write_refusal(root).expect("must refuse an un-ignored .claude/");
        assert!(why.contains("not gitignored"), "{why}");
    }

    /// `.claude` as a symlink (a junction on Windows) is refused whatever the
    /// repo says: the workspace root's `.claude` links into
    /// `qontinui-claude-config/.claude/`, so the write lands in tracked source
    /// with no repo of that name being the cwd.
    #[test]
    fn a_symlinked_dot_claude_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("workspace");
        let target = tmp.path().join("real-claude");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let link = root.join(".claude");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        #[cfg(windows)]
        {
            // A junction, not a symlink: `mklink /J` needs no elevation, and
            // `std` reports a mount-point reparse point as a symlink — which is
            // exactly the shape the workspace root has.
            let out = crate::process_helpers::no_window("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .output()
                .expect("mklink");
            assert!(
                out.status.success(),
                "mklink /J failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let why = claude_dir_write_refusal(&root).expect("must refuse a symlinked .claude");
        assert!(why.contains("symlink or junction"), "{why}");
    }

    /// A `.claude` that is a regular file is refused with a reason rather than
    /// an opaque IO error.
    #[test]
    fn a_dot_claude_that_is_a_file_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".claude"), "not a directory").unwrap();
        let why = claude_dir_write_refusal(tmp.path()).expect("must refuse");
        assert!(why.contains("not a directory"), "{why}");
    }

    // -- call-site parity ----------------------------------------------------

    /// Every `.rs` file under `src/`, recursively.
    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// `path → number of calls to `fn_name`` across the crate's non-test source.
    ///
    /// Three exclusions, each because it is not a spawn path: comment lines, a
    /// line that DEFINES a provisioner rather than calling one, and everything
    /// from the file's `#[cfg(test)] mod …` onward (which is where this very
    /// test lives).
    fn call_sites(fn_name: &str) -> BTreeMap<String, usize> {
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_sources(&src_root, &mut files);

        let needle = format!("{fn_name}(");
        let mut out = BTreeMap::new();
        for path in files {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let mut count = 0usize;
            let mut prev_was_cfg_test = false;
            for line in text.lines() {
                let trimmed = line.trim_start();
                let opens_a_module = trimmed.starts_with("mod ")
                    || trimmed.starts_with("pub mod ")
                    || trimmed.starts_with("pub(crate) mod ");
                if prev_was_cfg_test && opens_a_module {
                    break;
                }
                prev_was_cfg_test = trimmed == "#[cfg(test)]";
                if trimmed.starts_with("//") || trimmed.contains("fn provision_") {
                    continue;
                }
                count += trimmed.matches(&needle).count();
            }
            if count > 0 {
                let rel = path
                    .strip_prefix(&src_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, count);
            }
        }
        out
    }

    /// **The two provisioners must be called from the same set of spawn paths.**
    ///
    /// This is not hypothetical tidiness. The plan's own citation list named
    /// two of `provision_fleet_commands_for_session`'s three call sites and
    /// missed `looping_agent_supervisor.rs` — and because provisioning is
    /// fail-soft, implementing against the short list would have left every
    /// looping-agent session silently skill-less. Asserting the sets are equal
    /// is what stops the next spawn path added from drifting them apart.
    #[test]
    fn the_two_provisioners_are_called_from_the_same_spawn_paths() {
        let commands = call_sites("provision_fleet_commands_for_session");
        let skills = call_sites("provision_agent_skills_for_session");

        assert!(
            commands.values().sum::<usize>() >= 3,
            "the scanner found {} command-provisioning call site(s); the crate has at least \
             three, so the scan itself has gone stale and this test is proving nothing: {commands:?}",
            commands.values().sum::<usize>()
        );
        assert_eq!(
            commands, skills,
            "agent skills and fleet commands must be provisioned from the SAME spawn paths — \
             a session that gets one and not the other is silently under-equipped, because \
             both provisioners are fail-soft and log nothing a caller reads"
        );
    }
}
