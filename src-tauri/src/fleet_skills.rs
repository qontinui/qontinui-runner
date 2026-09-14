//! The agent SKILLS this binary ships, and their provisioning into a spawned
//! session's working directory.
//!
//! Sibling of [`crate::fleet_commands`], which does the same job for
//! `.claude/commands/*.md`. `claude` discovers project skills from
//! `<cwd>/.claude/skills/<name>/SKILL.md`, and on a device with no
//! `qontinui-claude-config` checkout there is no such directory at all — so the
//! fleet skills (`coord-revive`, `preflight`, `pr-status`, …) are unresolvable
//! there. This module BUNDLES them into the runner binary and writes them into
//! the session cwd.
//!
//! ## Why a `Dir`, not a `&[(&str, &str)]` roster
//!
//! A command is one markdown file, so [`crate::fleet_commands::FLEET_COMMANDS`]
//! can be a flat `(name, body)` list. A skill is a DIRECTORY — a mandatory
//! `SKILL.md` plus any number of helper scripts (`coord-pr-label` ships two
//! `.sh` files). Flattening that into a hand-maintained roster would mean one
//! `include_str!` per file and a roster edit for every helper script added.
//! [`include_dir`] embeds the tree instead, so **adding a skill is adding a
//! directory under `src-tauri/src/fleet_skills/` and nothing else** — no Rust
//! edit at all. The same crate already backs `spec_api::storage::EMBEDDED_PAGES`.
//!
//! ## The files in `fleet_skills/` are a RENDER, not the source of truth
//!
//! They are ordinary files in this public repository, reviewed through a normal
//! pull request, with git history as the tamper record — and that is why the
//! tree is checked in rather than fetched. But the CONTENT originates
//! elsewhere: `qontinui-claude-config/.claude/skills/**` is the SOURCE humans
//! edit, and every runner-side commit in this class is explicitly a *carry*
//! (`01c04a58`, "carry the coord-revive credential-door change into the bundled
//! copy"). Edit a skill THERE and copy it forward; an edit made directly here
//! is drift the moment it lands, and it is drift the source side will overwrite.
//!
//! An earlier version of this paragraph called these files "the CANONICAL
//! sources", which told a contributor to do the opposite. The direction, the
//! evidence for it, and the publish-cycle backstop that now detects a divergence
//! are in `crate::fleet`, in the block above `qontinui_root()`.
//!
//! ## Executable bits — embedded only
//!
//! `include_dir` embeds CONTENTS, not permissions, and git only records one
//! mode bit. A helper script written out with the default mode is not
//! executable on Unix, so a skill that shells out to it fails at the point of
//! use rather than at provisioning. [`provision_fleet_skills_into`] therefore
//! sets mode `0o755` on every `.sh` it writes **from the embedded tree** (Unix
//! only — Windows has no executable bit and `bash script.sh` works regardless).
//!
//! A file that came from the account layer is written [`PROVISIONED_FILE_MODE`]
//! — `0o644`, **no executable bit for owner, group or other**. Scripts in this
//! corpus are invoked as `bash <path>/<script>.sh`, which needs no `+x`, and
//! keeping the bit off is what stops "account-supplied text written to disk"
//! from becoming "account-supplied program registered with the OS". The
//! embedded tree is exempt because it is reviewed source in THIS repository
//! rather than text a backend handed this device.
//!
//! ## The account-override layer — it exists now
//!
//! This paragraph used to say skills had no fetch layer because "the
//! server-side surface for that does not exist". That stopped being true on
//! **2026-09-02**, when qontinui-web#1071 merged. The surface is
//! `GET {api_base}/api/v1/agent-text-units?kind=skill&invocable_only=true`
//! (with an index projection at `…/agent-text-units/index`), authenticated with
//! the user access token — the same credential [`crate::agent_commands`] uses.
//! A unit's `files` is a JSONB map of **relative path → text**, so a multi-file
//! skill (`SKILL.md` plus a helper `.sh`) is representable in one row, which is
//! the property a single-body `agent_commands` row could not express and the
//! reason no such layer existed before.
//!
//! So skills now resolve the same three rungs the commands do —
//! `fresh fetch → disk cache → embedded default`, in [`crate::agent_skills`] —
//! and this embedded tree is the **offline floor** rather than the only copy a
//! session can read. That is what ends the re-vendor treadmill: a drifted
//! bundle degrades a served device to stale-by-one-fetch instead of being the
//! only thing it can ever see. The floor still matters and still ships, and the
//! bodies must stay free of any one operator's absolute paths — see
//! [`tests::bundled_skills_have_no_operator_local_paths`].
//!
//! Plan `2026-08-20-fleet-served-agent-skills`, Phases 4 and 6.

use std::path::Path;

use include_dir::{include_dir, Dir};
use tracing::{info, warn};

use crate::agent_skills::{AgentSkillRegistry, ResolvedSkill};
use crate::capability_manifest::{self, CapabilityObservation, ProvisionReport};

/// The embedded skill tree. Each immediate subdirectory is one skill, named by
/// the directory (`coord-revive/` -> the `coord-revive` skill) and required to
/// contain a `SKILL.md` — the file `claude` reads to discover it.
pub(crate) static FLEET_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/fleet_skills");

/// The filename `claude` requires in each skill directory.
const SKILL_MANIFEST: &str = "SKILL.md";

/// The mode a file from the ACCOUNT layer is given on Unix: owner-writable,
/// world readable, and **no executable bit anywhere**. See the module docs.
// Windows has no executable bit, so `apply_mode` is a no-op there and this
// constant is read only by the cross-platform test that pins its value.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const PROVISIONED_FILE_MODE: u32 = 0o644;

/// The embedded defaults as the resolver's own [`ResolvedSkill`] shape — one
/// entry per immediate subdirectory of [`FLEET_SKILLS`], carrying every file
/// underneath it at its tree-relative path.
///
/// A file whose bytes are not UTF-8 is DROPPED with a warning rather than
/// guessed at: the served half of this corpus is a `files` map of
/// `path -> text`, so a non-text file has no representation in the layer this
/// floor has to be interchangeable with. Nothing in the shipped bundle is
/// non-UTF-8 today (measured 2026-09-14: 15 files, all `.md`/`.sh`), and
/// [`tests::every_embedded_file_is_text`] is what keeps that true.
pub(crate) fn embedded_skills() -> Vec<ResolvedSkill> {
    let mut out = Vec::new();
    for skill in FLEET_SKILLS.dirs() {
        let Some(name) = skill.path().file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mut files = qontinui_types::agent_text_units::AgentTextUnitFiles::new();
        collect_text_files(skill, skill.path(), &mut files);
        if files.is_empty() {
            warn!("fleet_skills: embedded skill {name:?} carries no readable file — skipping it");
            continue;
        }
        out.push(ResolvedSkill {
            name: name.to_string(),
            files,
            source: crate::agent_skills::AgentSkillSource::Builtin,
        });
    }
    out
}

/// Every UTF-8 file under `dir`, keyed by its path RELATIVE to `root` — the key
/// space a `files` map uses, and the one the provisioner joins under
/// `.claude/skills/<name>/`.
fn collect_text_files(
    dir: &Dir<'_>,
    root: &Path,
    out: &mut qontinui_types::agent_text_units::AgentTextUnitFiles,
) {
    for file in dir.files() {
        let Ok(rel) = file.path().strip_prefix(root) else {
            continue;
        };
        // `include_dir` paths use the host separator; the `files` key space is
        // `/`-separated, and `validate_agent_text_unit_file_path` rejects a
        // backslash outright.
        let key = rel.to_string_lossy().replace('\\', "/");
        match file.contents_utf8() {
            Some(text) => {
                out.insert(key, text.to_string());
            }
            None => warn!(
                "fleet_skills: embedded file {} is not UTF-8 — skipping it (the served \
                 layer this floor mirrors carries text only)",
                file.path().display()
            ),
        }
    }
    for sub in dir.dirs() {
        collect_text_files(sub, root, out);
    }
}

/// Provision the RESOLVED agent skills into `<workdir>/.claude/skills/<name>/`
/// so a `claude` session spawned with `workdir` as its cwd can resolve them as
/// PROJECT-scoped skills — even on a device with no `qontinui-claude-config`
/// checkout.
///
/// The set written is [`crate::agent_skills::resolve_registry`]'s output: the
/// account's skills where it has any, the embedded defaults otherwise.
///
/// Fail-soft, mirroring
/// [`crate::fleet_commands::provision_fleet_commands_for_session`]: any IO error
/// is logged via `tracing::warn!` and swallowed, because a provisioning failure
/// must never abort an otherwise-launchable spawn (the session simply lacks the
/// skills, which is the state every session was in before this module existed).
/// Resolution is fail-soft too: a failed fetch, a rejected credential, a
/// malformed unit, or a broken cache each degrade one step and warn, never
/// propagate.
///
/// Idempotent, and existing files are overwritten — EXCEPT where the
/// destination is already tracked by the enclosing git repository, which is
/// skipped (see [`provision_fleet_skills_into`] and
/// [`crate::provision_guard`]). **A tracked file outranks a served override**:
/// the account layer decides what this binary would write, not whether it may
/// replace a repository's committed content.
pub(crate) fn provision_fleet_skills_for_session(workdir: &str) {
    let registry = crate::agent_skills::resolve_registry();
    let skills_dir = Path::new(workdir).join(".claude").join("skills");

    // The registry's own row: WHICH of `resolve_registry`'s three arms answered.
    // Recorded before the write, because it is a fact about resolution rather
    // than about provisioning and holds even if every write below is skipped.
    let arm = registry.resolution_arm();
    crate::capability_manifest::record_observation(
        "agent_skills_registry",
        CapabilityObservation::new(capability_manifest::Rung::from(arm)).with_detail(format!(
            "AgentSkillSource::{} — {} account skill(s) over {} embedded default(s)",
            arm.as_str(),
            registry.override_count(),
            registry.builtin_count(),
        )),
    );

    match provision_fleet_skills_into(&skills_dir, &registry) {
        Ok(report) => crate::capability_manifest::record_provision(workdir, report),
        Err(e) => {
            // The destination directory itself could not be created, so no file
            // was even attempted. Fail-soft as before — the spawn continues —
            // but the degradation is now a ROW, not only a log line.
            warn!(
                "fleet_skills: failed to provision skills into {} \
                 (continuing spawn; the fleet skills may not resolve): {e}",
                skills_dir.display()
            );
            let mut report = ProvisionReport::new(
                "fleet_skills",
                registry.resolved_file_count(),
                capability_manifest::Rung::Unresolved,
            )
            .with_destination(skills_dir.display().to_string());
            report.skip(
                skills_dir.display().to_string(),
                capability_manifest::SkipReason::WriteFailed(e.to_string()),
            );
            crate::capability_manifest::record_provision(workdir, report);
        }
    }
}

/// Number of embedded SKILLS — immediate subdirectories of [`FLEET_SKILLS`],
/// one per skill.
pub(crate) fn embedded_skill_count() -> usize {
    FLEET_SKILLS.dirs().count()
}

/// Number of embedded FILES across every skill directory.
///
/// **Not the same number as [`embedded_skill_count`], and the difference is why
/// this function exists.** A skill is a directory: a mandatory `SKILL.md` plus
/// any helper scripts, so the bundle is more files than skills — and a report
/// measured in skills-embedded against files-written is a category error that
/// reads as a permanent shortfall.
///
/// Test-only now: the provisioner's roster is the RESOLVED file count
/// ([`AgentSkillRegistry::resolved_file_count`]), which differs from this the
/// moment an account skill replaces or adds one. What this still pins is that
/// [`embedded_skills`] carries every file the `include_dir!` tree holds.
#[cfg(test)]
fn embedded_skill_file_count() -> usize {
    fn count(dir: &Dir<'_>) -> usize {
        dir.files().count() + dir.dirs().map(count).sum::<usize>()
    }
    count(&FLEET_SKILLS)
}

/// Core of [`provision_fleet_skills_for_session`]: create `skills_dir` and write
/// every resolved skill under `<skills_dir>/<name>/`, returning the counts.
/// Split out so a unit test can drive it against a tempdir, mirroring
/// `fleet_commands::provision_fleet_commands_into`.
///
/// Idempotent (a second pass overwrites rather than errors), with ONE
/// exception: a destination that already exists AND is tracked in the enclosing
/// git repository is skipped, logged at `info!`, and counted in
/// [`ProvisionReport::skipped`] WITH its reason. Where the spawn cwd is a
/// checkout that tracks the destination path, an unconditional write silently
/// replaces the repo's own content and dirties its tree — and that outranks the
/// account layer: a served override never buys the right to overwrite committed
/// content.
///
/// Overwrite-idempotent, and deliberately **not** a mirror: a file that exists
/// on disk but is absent from the resolved bundle is left alone. Deleting a
/// subtree of a session's `.claude/skills/` on the strength of a remote list is
/// a much larger hazard than a stale sibling file, and the resolution chain
/// already replaces a skill's WHOLE bundle rather than merging into it.
///
/// **Every name and relative path is re-validated here** even though
/// [`crate::agent_skills::validate_override`] already did: this function takes
/// any registry, so the traversal refusal has to hold at the layer that
/// actually joins the path. A skill with a bad name or any bad path is skipped
/// ENTIRELY rather than partially written — a half-written skill is a skill
/// whose `SKILL.md` cites files that are not there.
///
/// **Fail-soft, and this is a hard requirement.** The tracked probe
/// ([`crate::provision_guard::TrackedPaths::probe`]) resolves EVERY failure — an
/// unreadable or absent git dir, no `git` binary, any non-zero exit, and a `git`
/// that hangs — to "nothing tracked", i.e. to writing exactly as before. A
/// skipped write must never become an aborted spawn, and a failed or slow probe
/// must never become one either. The probe runs ONCE for the whole tree, not
/// once per file, so this costs one process spawn rather than ~15.
fn provision_fleet_skills_into(
    skills_dir: &Path,
    registry: &AgentSkillRegistry,
) -> std::io::Result<ProvisionReport> {
    use qontinui_types::agent_text_units::{
        validate_agent_text_unit_file_path, validate_agent_text_unit_name,
    };

    std::fs::create_dir_all(skills_dir)?;
    let tracked = crate::provision_guard::TrackedPaths::probe(skills_dir);
    let resolved = registry.all();
    // The FILES are the embedded defaults unless an account skill replaced one;
    // the rung of the account layer itself is the separate
    // `agent_skills_registry` row, which the caller records. Same split as
    // `fleet_commands`.
    let mut out = ProvisionReport::new(
        "fleet_skills",
        registry.resolved_file_count(),
        capability_manifest::Rung::Embedded,
    )
    .with_destination(skills_dir.display().to_string())
    .with_detail(format!(
        "{} skill(s) resolved — {} embedded default(s), {} account skill(s)",
        resolved.len(),
        registry.builtin_count(),
        registry.override_count()
    ));

    for skill in &resolved {
        if let Err(e) = validate_agent_text_unit_name(&skill.name) {
            warn!(
                "fleet_skills: refusing to provision skill {:?} — {e}",
                skill.name
            );
            out.skip(
                skill.name.clone(),
                capability_manifest::SkipReason::Rejected(e.to_string()),
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
            out.skip(
                skill.name.clone(),
                capability_manifest::SkipReason::Rejected(format!(
                    "file path {bad:?} is not a safe relative path"
                )),
            );
            continue;
        }

        for (rel_path, text) in &skill.files {
            // `include_dir` and the `files` map share one key space, relative to
            // the skills dir — which is exactly what `TrackedPaths` reports.
            let relative = Path::new(skill.dir_name()).join(rel_path);
            let dst = skills_dir.join(&relative);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if tracked.should_skip(&dst, &relative) {
                info!(
                    "fleet_skills: skipping {} — it is tracked by the enclosing git \
                     repository, and overwriting it would silently replace that repo's \
                     own content and dirty its tree",
                    dst.display()
                );
                out.skip(
                    relative.display().to_string(),
                    capability_manifest::SkipReason::GitTracked,
                );
                continue;
            }
            std::fs::write(&dst, text)?;
            apply_mode(&dst, skill.source)?;
            out.record_written();
        }
    }

    if out.written == 0 {
        // Nothing landed at all — a stated outcome, not a claim that the
        // embedded floor delivered.
        out.set_rung(capability_manifest::Rung::Unresolved);
    }
    Ok(out)
}

/// Set the provisioned file's mode: `0o755` for a shell script THIS BINARY
/// carries, [`PROVISIONED_FILE_MODE`] for anything the account layer supplied.
///
/// `include_dir` carries no permission bits, so without the first arm a helper
/// script provisioned onto a Unix fleet device is non-executable and the skill
/// fails when it shells out. The second arm is the security property: see the
/// module docs.
#[cfg(unix)]
fn apply_mode(path: &Path, source: crate::agent_skills::AgentSkillSource) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let is_script = path.extension().and_then(|e| e.to_str()) == Some("sh");
    let mode = if is_script && !source.is_account_supplied() {
        0o755
    } else {
        PROVISIONED_FILE_MODE
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Windows has no executable bit; `bash script.sh` runs regardless.
#[cfg(not(unix))]
fn apply_mode(_path: &Path, _source: crate::agent_skills::AgentSkillSource) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_skills::tests::{bundle, skill_unit};
    use crate::agent_skills::{AgentSkillSource, ResolvedSkill};

    #[test]
    fn provisions_every_embedded_skill_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let out = provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new())
            .expect("provision");

        // Every embedded file lands, byte-identically to what `include_dir`
        // embedded, at the same relative path.
        let mut expected = 0usize;
        check_dir(&FLEET_SKILLS, &skills_dir, &mut expected);
        assert_eq!(
            out.written, expected,
            "written count should equal the embedded file count"
        );
        assert!(out.skipped.is_empty(), "nothing here is git-tracked");
        assert!(out.is_complete(), "a full pass must not read as degraded");
        assert_eq!(
            out.expected, expected,
            "the report's roster must be the embedded FILE count, not the skill count"
        );
        assert!(expected > 0, "the bundle should not be empty");
    }

    fn check_dir(dir: &Dir<'_>, dst_root: &std::path::Path, count: &mut usize) {
        for file in dir.files() {
            let dst = dst_root.join(file.path());
            assert!(dst.exists(), "{} should exist", dst.display());
            let on_disk = std::fs::read(&dst).expect("read provisioned file");
            assert_eq!(
                on_disk,
                file.contents(),
                "{} should be byte-identical to the embedded copy",
                dst.display()
            );
            *count += 1;
        }
        for sub in dir.dirs() {
            check_dir(sub, dst_root, count);
        }
    }

    #[test]
    fn every_embedded_skill_has_a_manifest() {
        let mut skills = 0usize;
        for skill in FLEET_SKILLS.dirs() {
            skills += 1;
            let name = skill
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unnamed>");
            assert!(
                skill.get_file(skill.path().join(SKILL_MANIFEST)).is_some(),
                "bundled skill {name} has no {SKILL_MANIFEST} — `claude` discovers a skill \
                 by that file, so a directory without one is provisioned but invisible; \
                 add src-tauri/src/fleet_skills/{name}/{SKILL_MANIFEST}"
            );
        }
        assert!(
            skills > 0,
            "no skills embedded — either the bundle lost its skills or the \
             include_dir! root went stale"
        );
    }

    #[test]
    fn provision_is_idempotent_overwrite() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let first = provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new())
            .expect("first provision");
        assert!(first.skipped.is_empty(), "nothing here is git-tracked");

        // Corrupt one provisioned file, then re-provision: the second pass must
        // restore it rather than skip it as already-present.
        let victim = skills_dir.join("coord-revive").join(SKILL_MANIFEST);
        std::fs::write(&victim, b"CLOBBERED").expect("clobber");

        let second = provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new())
            .expect("second provision");
        assert_eq!(
            (first.written, first.skipped.len()),
            (second.written, second.skipped.len()),
            "both passes should write the same count"
        );

        let restored = std::fs::read(&victim).expect("read restored");
        assert_ne!(
            restored, b"CLOBBERED",
            "re-provisioning must overwrite a modified file, not leave it"
        );
    }

    /// A destination that is TRACKED by the enclosing git repository must be
    /// left alone: overwriting it would silently replace that repo's own
    /// content and dirty its tree. Same rule the sibling command provisioner
    /// enforces.
    #[test]
    fn a_git_tracked_destination_is_skipped_not_clobbered() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(skills_dir.join("coord-revive")).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        let tracked = skills_dir.join("coord-revive").join(SKILL_MANIFEST);
        std::fs::write(&tracked, b"# the repo's own skill\n").unwrap();
        crate::provision_guard::test_support::git_add(tmp.path(), &tracked);

        let out = provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new())
            .expect("provision");

        assert_eq!(out.skipped.len(), 1, "the tracked file should be skipped");
        // The reason, not just the count — that pairing is the deliverable.
        assert_eq!(
            out.skipped[0].reason,
            crate::capability_manifest::SkipReason::GitTracked
        );
        assert!(
            out.skipped[0].unit.ends_with(SKILL_MANIFEST),
            "the skipped unit must name the file, got {:?}",
            out.skipped[0].unit
        );
        assert!(out.is_degraded(), "a skipped unit means the pass degraded");
        assert!(
            out.written > 0,
            "every OTHER embedded file should still be written"
        );
        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "# the repo's own skill\n",
            "a tracked destination must keep the repo's content, not the embedded copy"
        );
    }

    /// The untracked arm: same repo, same directory, but the file is not in the
    /// index — so the pre-existing overwrite behaviour is unchanged. This is the
    /// arm that keeps a fresh agent worktree fully provisioned.
    #[test]
    fn an_untracked_destination_inside_a_repo_is_still_written() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(skills_dir.join("coord-revive")).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        let dst = skills_dir.join("coord-revive").join(SKILL_MANIFEST);
        std::fs::write(&dst, b"stale, untracked\n").unwrap();
        // Deliberately NOT `git add`ed.

        let out = provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new())
            .expect("provision");

        assert!(
            out.skipped.is_empty(),
            "nothing is tracked, so nothing is skipped"
        );
        let embedded = FLEET_SKILLS
            .get_file(std::path::Path::new("coord-revive").join(SKILL_MANIFEST))
            .expect("coord-revive/SKILL.md is embedded");
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            embedded.contents(),
            "an untracked destination is overwritten exactly as before"
        );
    }

    #[test]
    fn bundled_skills_have_no_operator_local_paths() {
        // Mirrors `fleet_commands::tests::staged_fleet_commands_have_no_plan_path_hardcodes`,
        // but scoped to genuinely OPERATOR-LOCAL absolutes. These bodies ship to
        // every fleet device, so a path rooted on one operator's machine is a
        // dead pointer everywhere else.
        //
        // Deliberately NOT forbidding `qontinui-dev-notes/plans` the way the
        // command guard does: five of these skills cite a design plan as further
        // reading, in the documented `<workspace-root>/…` form, and a citation is
        // not an instruction to read a path. See this module's PR for the
        // separate question of whether those citations should be slug-only.
        const FORBIDDEN: &[&str] = &[
            "D:/qontinui-root",
            "D:\\qontinui-root",
            "C:/Users/",
            "/home/",
        ];
        let mut checked = 0usize;
        let mut placeholders = 0usize;
        check_paths(&FLEET_SKILLS, FORBIDDEN, &mut checked, &mut placeholders);
        assert!(checked > 0, "no skill files scanned — guard went stale");
        // The exemption below is only auditable if it says what it admitted. A
        // control that leaves no artifact is indistinguishable from an absent
        // one, so print the count rather than exempting silently.
        println!(
            "bundled-skill operator-local-path guard: {checked} file(s) scanned, \
             {placeholders} documented placeholder(s) exempted"
        );
    }

    #[test]
    fn bundled_shell_scripts_are_lf_only() {
        // `include_dir` embeds the WORKING COPY bytes at build time. A `.sh`
        // carrying CRLF is a script that dies on every Linux fleet device with
        // `bad interpreter: /bin/bash^M` — and it fails at the point of USE, far
        // from the build that embedded it. `.gitattributes` (`* text=auto
        // eol=lf`) makes a fresh checkout LF, so CI would not reproduce a CRLF
        // authored on a Windows box with attributes disabled; this guard is what
        // catches it.
        let mut scripts = 0usize;
        check_lf(&FLEET_SKILLS, &mut scripts);
        assert!(
            scripts > 0,
            "no bundled shell scripts scanned — either the bundle lost its              helper scripts or this guard's extension probe went stale"
        );
    }

    fn check_lf(dir: &Dir<'_>, scripts: &mut usize) {
        for file in dir.files() {
            if file.path().extension().and_then(|e| e.to_str()) != Some("sh") {
                continue;
            }
            *scripts += 1;
            assert!(
                !file.contents().contains(&b'\r'),
                "bundled shell script {} contains a carriage return — it is written                  verbatim onto fleet devices and CRLF makes bash fail with                  `bad interpreter: /bin/bash^M`; re-save it LF-only",
                file.path().display()
            );
        }
        for sub in dir.dirs() {
            check_lf(sub, scripts);
        }
    }

    /// An ELIDED segment immediately after an operator-local prefix marks a
    /// documentation PLACEHOLDER, not a path rooted on anybody's machine.
    ///
    /// Two spellings, because documentation uses two: a named hole
    /// (`C:/Users/<windows-user>/…`) and an elision (`C:/Users/.../Temp/…`).
    /// They are the same claim — *some* user, unspecified — written two ways.
    ///
    /// This guard's PROPERTY is stated in its own doc comment: these bodies
    /// ship to every fleet device, so *a path rooted on one operator's machine*
    /// is a dead pointer everywhere else. Its TEST was a bare `contains`, and
    /// the two came apart on 2026-09-03. Syncing
    /// `coord-revive/coord-revive.sh` from qontinui-claude-config brought in:
    ///
    /// ```text
    /// # `C:/Users/<windows-user>/AppData/Local/Temp/tmp.ABC/c5/proj`. MSYS rewrites
    /// ```
    ///
    /// — an illustrative example, inside a comment, explaining how MSYS
    /// rewrites paths. It is rooted on nobody's machine and is a dead pointer
    /// for no one, yet the substring test flagged it. That is a proxy that has
    /// drifted from the property it stands for, and the fix is to test the
    /// property directly rather than bend the documentation to satisfy the
    /// proxy.
    ///
    /// The elision arm was added on 2026-09-05, by the same route and for the
    /// same reason: bundling `coord-revive/approval-half-test.sh` brought in
    ///
    /// ```text
    /// # `MSYS2_ENV_CONV_EXCL` naming it, MSYS rewrites it to `C:/Users/.../Temp/...`,
    /// ```
    ///
    /// — again an illustrative example, inside a comment, about MSYS path
    /// rewriting, and again rooted on nobody's machine. The property has not
    /// moved; only the spelling of the hole did.
    ///
    /// `C:/Users/<x>`, `C:/Users/...` and `/home/<user>` are generic;
    /// `C:/Users/spinak` and `/home/spinak` are exactly what this guard exists
    /// to catch, and both still fail — neither `s` nor `.` twice-over opens a
    /// hole. The two `D:` roots are unaffected by either arm: nothing is ever
    /// spelled `D:/qontinui-root<` or `D:/qontinui-root...`, so no exemption
    /// widens them.
    const PLACEHOLDER_OPEN: u8 = b'<';

    /// The other spelling of the same hole: `.../` elides one or more segments.
    /// Three dots, not two — `..` is an ordinary relative-path component and
    /// admitting it would exempt a real path that merely walks upward.
    const PLACEHOLDER_ELISION: &str = "...";

    /// Does the text FOLLOWING an operator-local prefix open a documented hole
    /// rather than continue a concrete path?
    fn is_documented_placeholder(rest: &str) -> bool {
        rest.as_bytes().first() == Some(&PLACEHOLDER_OPEN) || rest.starts_with(PLACEHOLDER_ELISION)
    }

    fn check_paths(
        dir: &Dir<'_>,
        forbidden: &[&str],
        checked: &mut usize,
        placeholders: &mut usize,
    ) {
        for file in dir.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            for pat in forbidden {
                for (idx, _) in text.match_indices(pat) {
                    let rest = &text[idx + pat.len()..];
                    if is_documented_placeholder(rest) {
                        *placeholders += 1;
                        continue;
                    }
                    panic!(
                        "bundled skill file {} contains operator-local path {pat:?} — it ships \
                         to every fleet device, where that path does not exist; rewrite it in \
                         src-tauri/src/fleet_skills/{}. (A documented placeholder — {pat:?}<name> \
                         or the elided {pat:?}... — is exempt; this match was a concrete path.)",
                        file.path().display(),
                        file.path().display()
                    );
                }
            }
            *checked += 1;
        }
        for sub in dir.dirs() {
            check_paths(sub, forbidden, checked, placeholders);
        }
    }

    /// The exemption's NEGATIVE half, pinned directly rather than only in prose.
    ///
    /// A widening that quietly admitted a concrete path would leave every
    /// bundled body unguarded while the suite still went green, and the guard
    /// above cannot notice: it only ever sees the corpus, which today contains
    /// no operator-rooted path to catch it out. So state both directions on
    /// inputs the corpus does not supply.
    #[test]
    fn a_placeholder_is_a_hole_not_a_concrete_path() {
        // Holes, both spellings.
        assert!(is_documented_placeholder("<windows-user>/AppData"));
        assert!(is_documented_placeholder(".../Temp/..."));
        // Concrete paths — exactly what the guard exists to catch.
        assert!(!is_documented_placeholder("spinak/AppData"));
        assert!(!is_documented_placeholder("jspin"));
        // `..` is an ordinary relative component, not an elision.
        assert!(!is_documented_placeholder("../sibling"));
        // The prefix ending the file is a concrete-enough match to report.
        assert!(!is_documented_placeholder(""));
    }

    /// A bundled runbook that cites a sidecar the bundle does not carry is a
    /// dead pointer on exactly the device this module exists to serve.
    ///
    /// Measured 2026-09-05: `coord-revive/SKILL.md` said
    /// `Self-test: .claude/skills/coord-revive/approval-half-test.sh` while that
    /// file existed only in `qontinui-claude-config`. A device with no config
    /// checkout — the whole reason this module bundles anything — was
    /// provisioned the citation and not the file. That is the same class as a
    /// bundled copy drifting from its source (PR #1341), one step further out:
    /// not a stale file, an absent one.
    ///
    /// `qontinui-claude-config`'s `skill_bundle_unbundled` reports this from the
    /// OTHER side, but it is advisory there, never gates, and cannot run in this
    /// repository's CI at all — it needs both checkouts. This is the half that
    /// runs where the bundle is built.
    ///
    /// **Scope, deliberately.** Only citations into a skill the bundle ALREADY
    /// carries are enforced. A citation of a skill the runner does not bundle at
    /// all is a choice, not a defect — the same line the config-side check draws
    /// — so those are counted and printed rather than failed. Whether a cited
    /// file is one the skill NEEDS is a question about what its `SKILL.md`
    /// invokes, which is a parsing problem this guard declines just as its
    /// config-side sibling does; it states the fact and stops.
    #[test]
    fn bundled_skill_citations_resolve_inside_the_bundle() {
        let mut c = Citations::default();
        check_citations(&FLEET_SKILLS, &mut c);
        assert!(
            c.scanned > 0,
            "no skill files scanned — either the bundle lost its files or this \
             guard's traversal went stale"
        );
        // Same reason the operator-local-path guard prints its counts: a control
        // that leaves no artifact is indistinguishable from an absent one, and
        // this one's two exemptions are the part worth seeing.
        println!(
            "bundled-skill citation guard: {} file(s) scanned, {} in-bundle \
             citation(s) resolved, {} to skills this bundle does not carry, \
             {} naming a directory rather than a file (both out of scope by design)",
            c.scanned, c.resolved, c.foreign, c.directories
        );
    }

    /// What one pass of [`check_citations`] saw. A struct rather than four
    /// `&mut usize` because the two exemptions are only meaningful next to the
    /// count they were taken out of.
    #[derive(Default)]
    struct Citations {
        /// Files whose text was scanned for citations.
        scanned: usize,
        /// Citations naming a bundled skill's bundled file.
        resolved: usize,
        /// Citations naming a skill this bundle does not carry.
        foreign: usize,
        /// Citations naming a DIRECTORY inside a bundled skill. No skill is
        /// laid out that way today — [`SKILL_MANIFEST`] plus flat helper
        /// scripts is the shape — so this is a latent case rather than a live
        /// one, exempted so that adding a nested layout does not fail a guard
        /// whose two-component parser was never about nesting.
        directories: usize,
    }

    /// How a skill file spells a path to another file of the bundle — the
    /// location `claude` resolves a PROJECT skill at, and the location
    /// [`provision_fleet_skills_for_session`] writes this tree to.
    const SKILL_CITATION_PREFIX: &str = ".claude/skills/";

    fn check_citations(dir: &Dir<'_>, c: &mut Citations) {
        for file in dir.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            for (idx, _) in text.match_indices(SKILL_CITATION_PREFIX) {
                let rest = &text[idx + SKILL_CITATION_PREFIX.len()..];
                let Some((skill, sidecar)) = split_skill_citation(rest) else {
                    continue;
                };
                if FLEET_SKILLS.get_dir(skill).is_none() {
                    c.foreign += 1;
                    continue;
                }
                let rel = Path::new(skill).join(sidecar);
                if FLEET_SKILLS.get_file(&rel).is_some() {
                    c.resolved += 1;
                    continue;
                }
                if FLEET_SKILLS.get_dir(&rel).is_some() {
                    c.directories += 1;
                    continue;
                }
                panic!(
                    "bundled skill file {} cites {SKILL_CITATION_PREFIX}{skill}/{sidecar}, and \
                     '{skill}' IS bundled — without that file. Every spawned session is \
                     provisioned from this bundle, so on a device with no \
                     qontinui-claude-config checkout the citation resolves to nothing, which \
                     is the state this module exists to prevent. Add \
                     src-tauri/src/fleet_skills/{skill}/{sidecar}, or stop citing it.",
                    file.path().display()
                );
            }
            c.scanned += 1;
        }
        for sub in dir.dirs() {
            check_citations(sub, c);
        }
    }

    /// `(skill, sidecar)` out of the text following a `.claude/skills/` citation,
    /// or `None` when it names a directory rather than a file in one.
    ///
    /// A component runs to the first character a filename here does not use, so
    /// the surrounding prose ends it — a citation inside backticks, followed by
    /// a comma, or at the end of a sentence all yield the bare name. The one
    /// case that needs saying: a trailing `.` is TRIMMED rather than folded into
    /// the filename, because `…/coord-revive.sh.` closing a sentence names
    /// `coord-revive.sh`, and a guard that looked for `coord-revive.sh.` would
    /// report a missing file that is sitting right there.
    fn split_skill_citation(rest: &str) -> Option<(&str, &str)> {
        fn component(s: &str) -> &str {
            let end = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
                .unwrap_or(s.len());
            &s[..end]
        }
        let skill = component(rest);
        if skill.is_empty() {
            return None;
        }
        let sidecar = component(rest[skill.len()..].strip_prefix('/')?).trim_end_matches('.');
        if sidecar.is_empty() {
            return None;
        }
        Some((skill, sidecar))
    }

    /// The citation parser's own controls. The guard above can only ever fail
    /// on the corpus it is given; these pin what it makes of shapes the corpus
    /// happens not to contain today.
    #[test]
    fn a_skill_citation_parses_to_its_two_components() {
        assert_eq!(
            split_skill_citation("coord-revive/coord-revive.sh, pinned equal"),
            Some(("coord-revive", "coord-revive.sh"))
        );
        assert_eq!(
            split_skill_citation("coord-revive/approval-half-test.sh`, on the guard"),
            Some(("coord-revive", "approval-half-test.sh"))
        );
        // A sentence-final citation names the file, not the file plus a period.
        assert_eq!(
            split_skill_citation("pr-status/pr-status.sh."),
            Some(("pr-status", "pr-status.sh"))
        );
        // A directory citation names no file, so there is nothing to resolve.
        assert_eq!(split_skill_citation("coord-pr-label/"), None);
        assert_eq!(split_skill_citation("coord-revive"), None);
        assert_eq!(split_skill_citation("coord-revive isn't a path"), None);
    }

    // -- the embedded floor as the resolver sees it --------------------------

    /// The embedded floor `crate::agent_skills` resolves over is exactly the
    /// `include_dir!` tree — same skills, same relative paths, same bytes.
    ///
    /// The two representations are separate code paths now (a `Dir` walk and a
    /// `files` map), so a drift between them would silently change what a
    /// device with no account receives while every other test still passed.
    #[test]
    fn embedded_skills_mirror_the_include_dir_tree() {
        let skills = embedded_skills();
        assert_eq!(
            skills.len(),
            embedded_skill_count(),
            "one resolved skill per embedded directory"
        );
        let total: usize = skills.iter().map(|s| s.files.len()).sum();
        assert_eq!(
            total,
            embedded_skill_file_count(),
            "every embedded FILE must appear in the resolved floor"
        );
        for skill in &skills {
            assert_eq!(skill.source, AgentSkillSource::Builtin);
            for (rel, text) in &skill.files {
                let embedded = FLEET_SKILLS
                    .get_file(std::path::Path::new(&skill.name).join(rel))
                    .unwrap_or_else(|| panic!("{}/{rel} should be embedded", skill.name));
                assert_eq!(
                    embedded.contents_utf8(),
                    Some(text.as_str()),
                    "{}/{rel} must mirror the embedded bytes",
                    skill.name
                );
            }
        }
    }

    /// Every embedded file is UTF-8 text. [`embedded_skills`] DROPS anything
    /// that is not, because the served layer this floor mirrors carries text
    /// only — so a binary asset added to the tree would be written by the old
    /// `Dir` walk and silently absent from the resolved floor. This is the
    /// guard that turns that into a failing test instead of a missing file.
    #[test]
    fn every_embedded_file_is_text() {
        fn walk(dir: &Dir<'_>, seen: &mut usize) {
            for file in dir.files() {
                assert!(
                    file.contents_utf8().is_some(),
                    "embedded skill file {} is not UTF-8; the resolved floor carries \
                     text only, so it would be provisioned by nothing. Keep the bundle \
                     text-only, or teach `embedded_skills` a byte-carrying representation.",
                    file.path().display()
                );
                *seen += 1;
            }
            for sub in dir.dirs() {
                walk(sub, seen);
            }
        }
        let mut seen = 0usize;
        walk(&FLEET_SKILLS, &mut seen);
        assert!(seen > 0, "no embedded files scanned — guard went stale");
    }

    /// Whatever this binary embeds must satisfy every rule a SERVED unit does.
    /// An embedded default the resolver would reject is a default nobody can
    /// ever receive over the account layer, and it is also a bundle whose own
    /// self-references do not survive provisioning (`agent_skills::self_path`).
    #[test]
    fn embedded_skills_are_provisionable() {
        for skill in embedded_skills() {
            let unit = skill_unit(&skill.name, skill.files.clone());
            crate::agent_skills::validate_override(&unit, AgentSkillSource::Served).unwrap_or_else(
                |e| panic!("embedded skill {:?} is not provisionable: {e}", skill.name),
            );
        }
    }

    // -- the served layer ----------------------------------------------------

    /// A registry over the SHIPPED floor plus one account skill that replaces a
    /// bundled one BY NAME.
    fn registry_with(
        units: Vec<qontinui_types::agent_text_units::AgentTextUnit>,
    ) -> AgentSkillRegistry {
        let mut registry = AgentSkillRegistry::new();
        let accepted = registry.set_overrides(units, AgentSkillSource::Served);
        assert!(accepted > 0, "the fixture's units must all validate");
        registry
    }

    /// A served override REPLACES the embedded default of the same name, whole
    /// bundle and all — the default's sibling helper scripts do not survive as
    /// orphans beside it on a fresh provision.
    #[test]
    fn a_served_override_replaces_the_embedded_skill_by_name() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let registry = registry_with(vec![skill_unit(
            "coord-revive",
            bundle(&[("SKILL.md", "# my own coord-revive\n")]),
        )]);
        let out = provision_fleet_skills_into(&skills_dir, &registry).expect("provision");
        assert!(out.skipped.is_empty(), "nothing here is git-tracked");

        let dir = skills_dir.join("coord-revive");
        assert_eq!(
            std::fs::read_to_string(dir.join(SKILL_MANIFEST)).unwrap(),
            "# my own coord-revive\n",
            "the served body must win over the embedded default"
        );
        assert!(
            !dir.join("coord-revive.sh").exists(),
            "an override replaces the default's WHOLE bundle; a partial merge would \
             leave the default's stale sibling scripts beside the new SKILL.md"
        );
        // Every OTHER embedded skill is untouched by the override.
        assert!(skills_dir.join("preflight").join(SKILL_MANIFEST).exists());
    }

    /// A served skill with no embedded counterpart is additive.
    #[test]
    fn a_served_skill_with_no_embedded_counterpart_is_additive() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let registry = registry_with(vec![skill_unit(
            "account-only-skill",
            bundle(&[("SKILL.md", "# account only\n")]),
        )]);
        provision_fleet_skills_into(&skills_dir, &registry).expect("provision");
        assert_eq!(
            std::fs::read_to_string(skills_dir.join("account-only-skill").join(SKILL_MANIFEST))
                .unwrap(),
            "# account only\n"
        );
    }

    /// A skill bundle carrying a subdirectory lands as a subdirectory.
    #[test]
    fn nested_relative_paths_land_under_the_skill_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let registry = registry_with(vec![skill_unit(
            "nested-probe",
            bundle(&[
                ("SKILL.md", "# nested-probe\n"),
                ("reference/verdicts.md", "# verdicts\n"),
            ]),
        )]);
        provision_fleet_skills_into(&skills_dir, &registry).expect("provision");
        assert!(skills_dir
            .join("nested-probe")
            .join("reference")
            .join("verdicts.md")
            .exists());
    }

    /// **A tracked file outranks a served override.** The account layer decides
    /// what this binary would write, never whether it may replace a
    /// repository's committed content — so the repo's own `SKILL.md` survives
    /// an override that names it, and the skip is reported WITH its reason.
    #[test]
    fn a_tracked_destination_outranks_a_served_override() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(skills_dir.join("coord-revive")).unwrap();
        crate::provision_guard::test_support::git_init(tmp.path());

        let tracked = skills_dir.join("coord-revive").join(SKILL_MANIFEST);
        std::fs::write(&tracked, b"# the repo's own skill\n").unwrap();
        crate::provision_guard::test_support::git_add(tmp.path(), &tracked);

        let registry = registry_with(vec![skill_unit(
            "coord-revive",
            bundle(&[("SKILL.md", "# the account's override\n")]),
        )]);
        let out = provision_fleet_skills_into(&skills_dir, &registry).expect("provision");

        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "# the repo's own skill\n",
            "a tracked destination must keep the repo's content, override or not"
        );
        assert!(
            out.skipped.iter().any(|s| s.reason
                == crate::capability_manifest::SkipReason::GitTracked
                && s.unit.ends_with(SKILL_MANIFEST)),
            "the skip must be reported with its reason, got {:?}",
            out.skipped
        );
        assert!(out.is_degraded(), "a skipped unit means the pass degraded");
    }

    /// **Falsification gate, at the layer that joins the path.** A registry
    /// carrying a traversal path — built directly, bypassing
    /// `validate_override` — must write nothing at all for that skill, and in
    /// particular nothing outside the skill's own directory.
    #[test]
    fn the_provisioner_refuses_a_traversal_path_it_is_handed_directly() {
        for bad in [
            "../../ESCAPED.md",
            "../ESCAPED.md",
            "..\\..\\ESCAPED.md",
            "a/../../../ESCAPED.md",
            "/etc/passwd",
            "C:/ESCAPED.md",
        ] {
            let tmp = tempfile::tempdir().expect("create tempdir");
            let skills_dir = tmp.path().join(".claude").join("skills");

            let mut registry = AgentSkillRegistry::from_embedded(&[]);
            registry.set_unvalidated_overrides(vec![ResolvedSkill {
                name: "evil".to_string(),
                files: bundle(&[("SKILL.md", "# evil\n"), (bad, "pwned\n")]),
                source: AgentSkillSource::Served,
            }]);

            let out = provision_fleet_skills_into(&skills_dir, &registry).expect("provision");
            assert_eq!(
                out.written, 0,
                "{bad:?}: a skill with a traversal path must be skipped ENTIRELY, \
                 not partially written"
            );
            assert!(
                matches!(
                    out.skipped.first().map(|s| &s.reason),
                    Some(crate::capability_manifest::SkipReason::Rejected(_))
                ),
                "{bad:?}: the refusal must be reported with a reason, got {:?}",
                out.skipped
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
                !skills_dir.join("evil").join(SKILL_MANIFEST).exists(),
                "{bad:?}: the good half of a bad bundle must not land either"
            );
        }
    }

    /// A skill whose NAME is a traversal is skipped too.
    #[test]
    fn the_provisioner_refuses_a_traversal_name_it_is_handed_directly() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");
        let mut registry = AgentSkillRegistry::from_embedded(&[]);
        registry.set_unvalidated_overrides(vec![ResolvedSkill {
            name: "../../evil".to_string(),
            files: bundle(&[("SKILL.md", "# evil\n")]),
            source: AgentSkillSource::Served,
        }]);
        let out = provision_fleet_skills_into(&skills_dir, &registry).expect("provision");
        assert_eq!(out.written, 0);
        assert!(!tmp.path().join("evil").exists());
    }

    /// Every file under `dir`, recursively — the sentinel sweep above.
    fn all_files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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

    // -- the executable bit --------------------------------------------------

    /// The mode account-supplied files are given carries no executable bit for
    /// any of owner, group or other. Expressible — and therefore asserted — on
    /// every platform, including the Windows boxes this fleet runs on where the
    /// end-to-end assertion below is compiled out.
    #[test]
    fn the_provisioned_file_mode_has_no_executable_bit() {
        assert_eq!(
            PROVISIONED_FILE_MODE & 0o111,
            0,
            "account-supplied files must never be executable: a `.sh` in this corpus is \
             run as `bash <path>`, and an exec bit would turn account-supplied text into \
             an account-supplied program"
        );
        assert_eq!(PROVISIONED_FILE_MODE, 0o644);
    }

    /// End to end on Unix: a script that arrived over the ACCOUNT layer is
    /// written non-executable, while the same filename coming out of the
    /// embedded tree keeps `0o755`. The difference is the whole rule — reviewed
    /// source in this repository may be a program; text a backend handed this
    /// device may not.
    #[cfg(unix)]
    #[test]
    fn a_served_script_is_not_executable_but_an_embedded_one_is() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        // The embedded floor alone.
        provision_fleet_skills_into(&skills_dir, &AgentSkillRegistry::new()).expect("provision");
        let embedded_script = skills_dir.join("coord-revive").join("coord-revive.sh");
        let mode = std::fs::metadata(&embedded_script)
            .expect("the embedded helper script should be provisioned")
            .permissions()
            .mode();
        assert_ne!(
            mode & 0o111,
            0,
            "an EMBEDDED `.sh` keeps its executable bit (mode {mode:o})"
        );

        // The same skill, now served.
        let served = tempfile::tempdir().expect("create tempdir");
        let served_dir = served.path().join(".claude").join("skills");
        let registry = registry_with(vec![skill_unit(
            "coord-revive",
            bundle(&[
                ("SKILL.md", "# coord-revive\n"),
                ("coord-revive.sh", "#!/usr/bin/env bash\necho hi\n"),
            ]),
        )]);
        provision_fleet_skills_into(&served_dir, &registry).expect("provision");
        for rel in [SKILL_MANIFEST, "coord-revive.sh"] {
            let path = served_dir.join("coord-revive").join(rel);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o111,
                0,
                "{rel} came from the account layer and must not be executable (mode {mode:o})"
            );
        }
    }
}
