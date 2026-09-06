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
///
/// Idempotent, and existing files are overwritten — EXCEPT where the
/// destination is already tracked by the enclosing git repository, which is
/// skipped (see [`provision_fleet_skills_into`] and
/// [`crate::provision_guard`]).
pub(crate) fn provision_fleet_skills_for_session(workdir: &str) {
    let skills_dir = Path::new(workdir).join(".claude").join("skills");
    match provision_fleet_skills_into(&skills_dir) {
        Ok(ProvisionedSkills { written, skipped }) => {
            info!(
                "fleet_skills: provisioned {written} file(s) into {} \
                 ({skipped} skipped as git-tracked; {} skill(s) embedded)",
                skills_dir.display(),
                embedded_skill_count(),
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

/// Outcome of one provisioning pass: how many files were written, and how many
/// were left alone because the destination is tracked by the enclosing git
/// repository. Sibling of [`crate::fleet_commands::Provisioned`]; the two are
/// kept separate so each provisioner owns its own summary and counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct ProvisionedSkills {
    /// Files written (overwriting whatever was there).
    pub(crate) written: usize,
    /// Files NOT written because the destination exists and is git-tracked.
    pub(crate) skipped: usize,
}

/// Core of [`provision_fleet_skills_for_session`]: create `skills_dir` and write
/// the whole embedded tree into it, returning the counts. Split out so a unit
/// test can drive it against a tempdir, mirroring
/// `fleet_commands::provision_fleet_commands_into`.
///
/// Idempotent (a second pass overwrites rather than errors), with ONE
/// exception: a destination that already exists AND is tracked in the enclosing
/// git repository is skipped, logged at `info!`, and counted in
/// [`ProvisionedSkills::skipped`]. Where the spawn cwd is a checkout that tracks
/// the destination path, an unconditional write silently replaces the repo's own
/// content and dirties its tree.
///
/// **Fail-soft, and this is a hard requirement.** The tracked probe
/// ([`crate::provision_guard::TrackedPaths::probe`]) resolves EVERY failure — an
/// unreadable or absent git dir, no `git` binary, any non-zero exit, and a `git`
/// that hangs — to "nothing tracked", i.e. to writing exactly as before. A
/// skipped write must never become an aborted spawn, and a failed or slow probe
/// must never become one either. The probe runs ONCE for the whole tree, not
/// once per file, so this costs one process spawn rather than ~13.
fn provision_fleet_skills_into(skills_dir: &Path) -> std::io::Result<ProvisionedSkills> {
    std::fs::create_dir_all(skills_dir)?;
    let tracked = crate::provision_guard::TrackedPaths::probe(skills_dir);
    let mut out = ProvisionedSkills::default();
    write_dir_recursive(&FLEET_SKILLS, skills_dir, &tracked, &mut out)?;
    Ok(out)
}

/// Write every file in `dir` (and its subdirectories) under `dst_root`,
/// preserving the tree shape. `include_dir` paths are already relative to the
/// embedded root, so they map straight onto the destination.
///
/// A destination that exists and is git-tracked is skipped rather than written;
/// see [`provision_fleet_skills_into`] for why, and for the fail-soft contract.
fn write_dir_recursive(
    dir: &Dir<'_>,
    dst_root: &Path,
    tracked: &crate::provision_guard::TrackedPaths,
    out: &mut ProvisionedSkills,
) -> std::io::Result<()> {
    for file in dir.files() {
        // `include_dir` paths are relative to the embedded root, which is
        // exactly the key space `TrackedPaths` reports (relative to `dst_root`).
        let dst = dst_root.join(file.path());
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if tracked.should_skip(&dst, file.path()) {
            info!(
                "fleet_skills: skipping {} — it is tracked by the enclosing git \
                 repository, and overwriting it would silently replace that repo's \
                 own content and dirty its tree",
                dst.display()
            );
            out.skipped += 1;
            continue;
        }
        std::fs::write(&dst, file.contents())?;
        set_executable_if_script(&dst)?;
        out.written += 1;
    }
    for sub in dir.dirs() {
        write_dir_recursive(sub, dst_root, tracked, out)?;
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

        let out = provision_fleet_skills_into(&skills_dir).expect("provision");

        // Every embedded file lands, byte-identically to what `include_dir`
        // embedded, at the same relative path.
        let mut expected = 0usize;
        check_dir(&FLEET_SKILLS, &skills_dir, &mut expected);
        assert_eq!(
            out.written, expected,
            "written count should equal the embedded file count"
        );
        assert_eq!(out.skipped, 0, "nothing here is git-tracked");
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
        assert_eq!(first.skipped, 0, "nothing here is git-tracked");

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

        let out = provision_fleet_skills_into(&skills_dir).expect("provision");

        assert_eq!(out.skipped, 1, "the tracked file should be skipped");
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

        let out = provision_fleet_skills_into(&skills_dir).expect("provision");

        assert_eq!(out.skipped, 0, "nothing is tracked, so nothing is skipped");
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
}
