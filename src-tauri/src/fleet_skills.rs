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
//! ## The files in `fleet_skills/` are the CANONICAL sources
//!
//! As with `fleet_commands/`, they are not staged copies of an upstream: they
//! are ordinary files in this public repository, reviewed through a normal pull
//! request, with git history as the tamper record.
//!
//! ## Executable bits
//!
//! `include_dir` embeds CONTENTS, not permissions, and git only records one
//! mode bit. A helper script written out with the default mode is not
//! executable on Unix, so a skill that shells out to it fails at the point of
//! use rather than at provisioning. [`provision_fleet_skills_into`] therefore
//! sets mode `0o755` on every `.sh` it writes (Unix only — Windows has no
//! executable bit and `bash script.sh` works regardless).
//!
//! ## No account-override layer, deliberately
//!
//! `crate::agent_commands` adds `fresh fetch → disk cache → embedded default`
//! on top of the command bundle. Skills have no such layer: an override would
//! have to carry a whole directory tree rather than one body, and the
//! server-side surface for that does not exist. Skills are therefore
//! embedded-only — which is the floor this module exists to provide, and the
//! reason the bodies must stay free of any one operator's absolute paths (see
//! [`tests::bundled_skills_have_no_operator_local_paths`]).

use std::path::Path;

use include_dir::{include_dir, Dir};
use tracing::{info, warn};

/// The embedded skill tree. Each immediate subdirectory is one skill, named by
/// the directory (`coord-revive/` -> the `coord-revive` skill) and required to
/// contain a `SKILL.md` — the file `claude` reads to discover it.
static FLEET_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/fleet_skills");

/// The filename `claude` requires in each skill directory.
const SKILL_MANIFEST: &str = "SKILL.md";

/// Provision the embedded skills into `<workdir>/.claude/skills/` so a `claude`
/// session spawned with `workdir` as its cwd can resolve them as PROJECT-scoped
/// skills — even on a device with no `qontinui-claude-config` checkout.
///
/// Fail-soft, mirroring
/// [`crate::fleet_commands::provision_fleet_commands_for_session`]: any IO error
/// is logged via `tracing::warn!` and swallowed, because a provisioning failure
/// must never abort an otherwise-launchable spawn (the session simply lacks the
/// skills, which is the state every session was in before this module existed).
/// Idempotent: existing files are overwritten.
pub(crate) fn provision_fleet_skills_for_session(workdir: &str) {
    let skills_dir = Path::new(workdir).join(".claude").join("skills");
    match provision_fleet_skills_into(&skills_dir) {
        Ok(written) => {
            info!(
                "fleet_skills: provisioned {written} file(s) across {} skill(s) into {}",
                embedded_skill_count(),
                skills_dir.display(),
            );
        }
        Err(e) => {
            warn!(
                "fleet_skills: failed to provision skills into {} \
                 (continuing spawn; the fleet skills may not resolve): {e}",
                skills_dir.display()
            );
        }
    }
}

/// Number of embedded skills (immediate subdirectories of [`FLEET_SKILLS`]).
fn embedded_skill_count() -> usize {
    FLEET_SKILLS.dirs().count()
}

/// Core of [`provision_fleet_skills_for_session`]: create `skills_dir` and write
/// the whole embedded tree into it (overwrite/idempotent), returning the count
/// of FILES written. Split out so a unit test can drive it against a tempdir,
/// mirroring `fleet_commands::provision_fleet_commands_into`.
fn provision_fleet_skills_into(skills_dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(skills_dir)?;
    let mut written = 0usize;
    write_dir_recursive(&FLEET_SKILLS, skills_dir, &mut written)?;
    Ok(written)
}

/// Write every file in `dir` (and its subdirectories) under `dst_root`,
/// preserving the tree shape. `include_dir` paths are already relative to the
/// embedded root, so they map straight onto the destination.
fn write_dir_recursive(dir: &Dir<'_>, dst_root: &Path, written: &mut usize) -> std::io::Result<()> {
    for file in dir.files() {
        let dst = dst_root.join(file.path());
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dst, file.contents())?;
        set_executable_if_script(&dst)?;
        *written += 1;
    }
    for sub in dir.dirs() {
        write_dir_recursive(sub, dst_root, written)?;
    }
    Ok(())
}

/// Mark `path` executable when it is a shell script. `include_dir` carries no
/// permission bits, so without this a helper script provisioned onto a Unix
/// fleet device is non-executable and the skill fails when it shells out.
#[cfg(unix)]
fn set_executable_if_script(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.extension().and_then(|e| e.to_str()) != Some("sh") {
        return Ok(());
    }
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

/// Windows has no executable bit; `bash script.sh` runs regardless.
#[cfg(not(unix))]
fn set_executable_if_script(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_every_embedded_skill_into_dir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let skills_dir = tmp.path().join(".claude").join("skills");

        let written = provision_fleet_skills_into(&skills_dir).expect("provision");

        // Every embedded file lands, byte-identically to what `include_dir`
        // embedded, at the same relative path.
        let mut expected = 0usize;
        check_dir(&FLEET_SKILLS, &skills_dir, &mut expected);
        assert_eq!(
            written, expected,
            "written count should equal the embedded file count"
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

        let first = provision_fleet_skills_into(&skills_dir).expect("first provision");

        // Corrupt one provisioned file, then re-provision: the second pass must
        // restore it rather than skip it as already-present.
        let victim = skills_dir.join("coord-revive").join(SKILL_MANIFEST);
        std::fs::write(&victim, b"CLOBBERED").expect("clobber");

        let second = provision_fleet_skills_into(&skills_dir).expect("second provision");
        assert_eq!(first, second, "both passes should write the same count");

        let restored = std::fs::read(&victim).expect("read restored");
        assert_ne!(
            restored, b"CLOBBERED",
            "re-provisioning must overwrite a modified file, not leave it"
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
        const FORBIDDEN: &[&str] = &["D:/qontinui-root", "D:\\qontinui-root", "C:/Users/", "/home/"];
        let mut checked = 0usize;
        check_paths(&FLEET_SKILLS, FORBIDDEN, &mut checked);
        assert!(checked > 0, "no skill files scanned — guard went stale");
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

    fn check_paths(dir: &Dir<'_>, forbidden: &[&str], checked: &mut usize) {
        for file in dir.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            for pat in forbidden {
                assert!(
                    !text.contains(pat),
                    "bundled skill file {} contains operator-local path {pat:?} — it ships \
                     to every fleet device, where that path does not exist; rewrite it in \
                     src-tauri/src/fleet_skills/{}",
                    file.path().display(),
                    file.path().display()
                );
            }
            *checked += 1;
        }
        for sub in dir.dirs() {
            check_paths(sub, forbidden, checked);
        }
    }
}
