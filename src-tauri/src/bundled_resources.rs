//! The runner's single door to "where is this crate-bundled asset at runtime?".
//!
//! Sibling of [`crate::workspace_paths`], which answers the *other* location
//! question ("where do the repo checkouts live?"). Keeping them separate is the
//! point: a file shipped **inside the installer** and a file that exists only in
//! a **developer checkout** are different things, and resolving one as the other
//! is a wrong answer that looks right on the author's machine.
//!
//! Plan: `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 5
//! Phase 6.
//!
//! ## What this replaces
//!
//! Two sites joined their asset path onto `env!("CARGO_MANIFEST_DIR")`. That is a
//! **compile-time** constant: it names the source tree the binary was BUILT
//! from, so it baked the build host's absolute layout into a shipped
//! open-source binary and, on any other machine, named a directory that does not
//! exist. `env_agent/collectors.rs` states the principle authoritatively:
//! *"Deliberately NOT the compile-time `CARGO_MANIFEST_DIR`. That path measures
//! which source tree the binary was BUILT from, not the box."*
//!
//! ## Why the bundle declaration is load-bearing
//!
//! Before this phase, `tauri.conf.json`'s `bundle` block declared **no
//! `resources` key at all** — `src-tauri/resources/` and `src-tauri/data/` were
//! tracked in git and shipped in no installer. So swapping `CARGO_MANIFEST_DIR`
//! for the resource resolver on its own would have replaced a path that exists
//! only on the build host with one that exists on **no** host. The same phase
//! added `"resources": ["resources/code-semantics/**/*", "data/**/*"]`;
//! [`resolve`] is only correct because of it.
//!
//! The first glob is narrow **on purpose**. Exactly one file under
//! `src-tauri/resources/` is resolved at runtime:
//! `code-semantics/ts-language-service.mjs`. The other eleven —
//! `intercept/*`, `session-restore/*` and `shell-integration.*` — are
//! `include_str!`'d into the binary at compile time
//! (`install_effects_producer::intercept::shim_materializer`,
//! `session::claude_hook`, `terminal::session`), so shipping them would put dead
//! copies in the install directory that no code reads and no edit affects: a
//! drift trap, not a safety margin. `data/**/*` stays broad because both
//! `runner_state_machine.json` and `htn_methods/` genuinely are resolved at
//! runtime (`unified_workflow_executor::types`).
//!
//! ## The dev rung is TWO rungs, and the exe-relative one comes first
//!
//! [`dev_checkout`] is often described as "the checkout you are running from".
//! That is what it must MEAN, but the workspace root alone cannot deliver it:
//! the configured `paths.workspace_root` setting outranks the exe-anchor walk
//! inside `qontinui_types::paths`, so a temp runner built and spawned from an
//! agent worktree (`<root>/_wt/<tag>/qontinui-runner`) resolves the root to the
//! CANONICAL checkout and loads its assets from there. An agent editing
//! `ts-language-service.mjs`, `data/runner_state_machine.json`,
//! `data/htn_methods/` or `tsconfig.json` in a worktree and then spawning a temp
//! runner to verify would exercise a different — possibly hundreds of commits
//! old — copy, silently. That is the fleet's normal dev loop, so it is the
//! common case, not an edge one.
//!
//! So [`dev_checkout`] tries the **exe-relative** copy first (derived from
//! `std::env::current_exe`, which for a dev build sits at
//! `<repo>/src-tauri/target/<profile>/`), and only then the workspace-root copy.
//! The workspace-root rung stays because an installed binary has no useful
//! ancestry. Both are existence-checked and both fail soft: any failure yields
//! `None`, never a panic and never the cwd.
//!
//! ## Tauri 2, not Tauri 1
//!
//! There is no `PathResolver::resolve_resource` in Tauri 2 (this crate pins
//! `tauri = "2.5"`). The equivalent is `Manager::path()` →
//! `PathResolver::resolve(path, BaseDirectory::Resource)`, which routes through
//! the bundler's own `resource_relpath` mapping — so the relative path used here
//! is the same one the bundler wrote, rather than a hand-rolled join that could
//! drift from it.

use std::path::{Path, PathBuf};

use crate::capability_manifest::{CapabilityObservation, Rung};

/// This repo's checkout directory name, for the dev rung's
/// `<workspace-root>/qontinui-runner/src-tauri/...` join.
const RUNNER_REPO_DIR: &str = "qontinui-runner";

/// Resolve `relative` against the directory Tauri unpacked `bundle.resources`
/// into.
///
/// `None` when the process-global `AppHandle` has not been set yet (unit tests,
/// early boot, any non-Tauri context — see [`crate::tauri_app_handle::current`])
/// or when the platform resolver cannot answer. Both are fall-throughs to the
/// next rung; this never panics and never unwraps.
///
/// Note this does **not** existence-check: a resolver answer for a bundle that
/// was never installed is a plausible path to a missing file. Pass the result
/// through [`first_existing`], which is where the check lives for every rung
/// alike.
pub fn resolve(relative: &Path) -> Option<PathBuf> {
    use tauri::path::BaseDirectory;
    use tauri::Manager;

    let app = crate::tauri_app_handle::current()?;
    app.path().resolve(relative, BaseDirectory::Resource).ok()
}

/// The dev-checkout copy of `relative` — the checkout this binary was actually
/// built from where that can be established, else the one under the resolved
/// workspace root.
///
/// This is what keeps a `cargo run` / `cargo test` session working with no
/// bundle present. Two rungs, in this order (see the module header for why the
/// order is load-bearing):
///
/// 1. **exe-relative** — the nearest ancestor of `current_exe()` that actually
///    holds `src-tauri/<relative>`. For a temp runner built in an agent
///    worktree this is the worktree, which is the tree the operator just edited.
/// 2. **workspace-root** — `<root>/qontinui-runner/src-tauri/<relative>`, via
///    [`crate::workspace_paths::workspace_root`], the crate's one door. Kept for
///    an installed binary, whose ancestry says nothing.
///
/// Both rungs fail soft: an unreadable `current_exe()` or an unresolved root
/// yields `None` (the latter with the rejected probe already logged), never a
/// guess and never the process's cwd.
pub fn dev_checkout(relative: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let root = crate::workspace_paths::workspace_root();
    dev_checkout_in(exe.as_deref(), root.as_deref(), relative)
}

/// Pure core of [`dev_checkout`] with both anchors injected, so the rung
/// ordering is unit-testable against a synthetic tree — no env read, no
/// dependency on how the machine running the suite is laid out.
fn dev_checkout_in(
    exe: Option<&Path>,
    workspace_root: Option<&Path>,
    relative: &Path,
) -> Option<PathBuf> {
    exe_relative_checkout(exe, relative)
        .or_else(|| workspace_root_checkout(workspace_root, relative))
}

/// The copy of `relative` under the resolved workspace root's checkout of THIS
/// repo: `<root>/qontinui-runner/src-tauri/<relative>`.
///
/// Deliberately **not** existence-checked — [`first_existing`] owns that for
/// every rung alike, and `dev_checkout`'s contract has always been that this
/// rung hands back a plausible path rather than a verified one. Split out of
/// [`dev_checkout_in`] so [`resolve_with_rung_in`] can name this rung
/// separately from the exe-relative one **without owning a second copy of the
/// join**: one join, two callers, so the two can never drift into disagreeing
/// about where the dev copy lives.
fn workspace_root_checkout(workspace_root: Option<&Path>, relative: &Path) -> Option<PathBuf> {
    workspace_root.map(|root| root.join(RUNNER_REPO_DIR).join("src-tauri").join(relative))
}

/// The copy of `relative` inside the checkout the running executable was built
/// in: the nearest ancestor of `exe` holding an existing `src-tauri/<relative>`.
///
/// Existence-checked, so an installed binary — or a dev exe asked for an asset
/// its own tree does not carry — simply misses and lets the caller fall to the
/// next rung. `None` on any failure; it never panics, never unwraps, and never
/// falls back to the cwd (a cwd-relative answer would make the asset a function
/// of how the process was launched, the exact defect this plan removes).
fn exe_relative_checkout(exe: Option<&Path>, relative: &Path) -> Option<PathBuf> {
    exe?.ancestors()
        .map(|dir| dir.join("src-tauri").join(relative))
        .find(|candidate| candidate.exists())
}

/// The first candidate that exists on disk, in the order given.
///
/// Pure: every candidate is injected, so a caller's rung ordering and the
/// existence rule are unit-testable against a temp layout — no env read, no
/// Tauri runtime, no dependency on how the machine running the suite happens to
/// be laid out.
///
/// Absent candidates are **skipped, not short-circuiting**, so a machine with no
/// `AppHandle` still reaches the dev rung. Likewise a candidate that is present
/// but does not exist on disk (a stale env override, an uninstalled bundle)
/// falls through rather than winning.
pub fn first_existing<const N: usize>(candidates: [Option<&Path>; N]) -> Option<PathBuf> {
    first_existing_indexed(candidates).path
}

// ===========================================================================
// Rung reporting — plan `2026-08-31-published-build-parity-check`, Phase 2.
//
// Purely additive. `resolve`, `dev_checkout` and `first_existing` keep their
// signatures and their behaviour to the byte; what is new is that the SAME walk
// can now say WHICH candidate answered and what it stepped over on the way.
//
// The reason that matters is this module's own opening principle: *"a file
// shipped inside the installer and a file that exists only in a developer
// checkout are different things, and resolving one as the other is a wrong
// answer that looks right on the author's machine."* Until now the return type
// was a `PathBuf` that looked identical either way, so the wrong answer and the
// right one were indistinguishable from outside. A rung makes them different
// values.
// ===========================================================================

/// Why one candidate in a rung-ordered list did not answer.
///
/// The two are different findings and are never collapsed. `Absent` says the
/// rung's own resolver produced nothing to try — no `AppHandle`, no readable
/// `current_exe()`, no resolved workspace root — so the rung was not *reached*.
/// `NotOnDisk` says the rung DID produce a candidate and the file is not there,
/// which on the bundle rung is a bundle defect: an installer that shipped a path
/// it does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateMiss {
    /// The rung offered no candidate at all.
    Absent,
    /// The rung named a path, and that path is not on disk.
    NotOnDisk(PathBuf),
}

impl CandidateMiss {
    /// One sentence fragment, to compose after a rung's wire name.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            CandidateMiss::Absent => {
                "offered no candidate (the rung's own resolver returned nothing)".to_string()
            }
            CandidateMiss::NotOnDisk(path) => format!("{} is not on disk", path.display()),
        }
    }
}

/// [`first_existing`]'s answer plus **which candidate produced it**, and what
/// every higher-priority candidate did instead.
///
/// `index` is the position in the caller's own array, so the caller — which owns
/// the rung ordering — maps it back to a rung. This type deliberately knows
/// nothing about rungs: it is the existence rule, and inventing a second copy of
/// somebody else's precedence order here is exactly what
/// [`crate::capability_manifest`]'s discipline (3) forbids.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FirstExisting {
    /// The path that answered, or `None` when no candidate existed.
    pub path: Option<PathBuf>,
    /// The index of the candidate that answered, `None` when none did. Always
    /// `Some` exactly when [`path`](Self::path) is `Some`.
    pub index: Option<usize>,
    /// Every candidate ahead of the winner, in priority order, and why it did
    /// not answer. When nothing answered this covers the whole list.
    ///
    /// **Populated even on success** — that is the point. A resolution that fell
    /// through the bundle rung to a dev checkout and one that had no bundle rung
    /// to try are the same `path` and completely different findings.
    pub misses: Vec<(usize, CandidateMiss)>,
}

/// [`first_existing`], reporting which candidate index answered.
///
/// Same walk, same rules: absent candidates are skipped rather than
/// short-circuiting, and a candidate that is present but not on disk falls
/// through rather than winning. The only difference is that the skips are
/// recorded instead of discarded.
pub fn first_existing_indexed<const N: usize>(candidates: [Option<&Path>; N]) -> FirstExisting {
    let mut misses: Vec<(usize, CandidateMiss)> = Vec::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        match candidate {
            Some(path) if path.exists() => {
                return FirstExisting {
                    path: Some(path.to_path_buf()),
                    index: Some(index),
                    misses,
                };
            }
            Some(path) => misses.push((index, CandidateMiss::NotOnDisk(path.to_path_buf()))),
            None => misses.push((index, CandidateMiss::Absent)),
        }
    }
    FirstExisting {
        path: None,
        index: None,
        misses,
    }
}

/// The rungs [`resolve_with_rung`] walks, best first. Index `i` of the
/// [`FirstExisting`] it builds is `CANDIDATE_RUNGS[i]`, which is the whole
/// mapping — there is no second table and no re-derived ordering.
///
/// The three are the module's own three, in the order the module already
/// resolved them:
///
/// 1. [`resolve`] — Tauri's `BaseDirectory::Resource`, i.e. the installer's
///    unpacked `bundle.resources` → [`Rung::BundleResource`].
/// 2. [`exe_relative_checkout`] — the checkout this binary was built in →
///    [`Rung::ExeRelativeCheckout`].
/// 3. [`workspace_root_checkout`] — `<workspace-root>/qontinui-runner/src-tauri`
///    → [`Rung::DevCheckout`].
///
/// Rungs 2 and 3 are the two halves [`dev_checkout`] returns as ONE
/// `Option<PathBuf>`. They are reported apart here because the module header
/// says they answer for different reasons — 2 is the tree the exe was built in
/// (an agent worktree, in this fleet's normal dev loop), 3 is whatever the
/// resolved workspace root points at, which for a temp runner is a DIFFERENT
/// and possibly hundreds-of-commits-stale checkout. A manifest that reported
/// both as one rung would erase precisely the distinction this module exists to
/// make.
const CANDIDATE_RUNGS: [Rung; 3] = [
    Rung::BundleResource,
    Rung::ExeRelativeCheckout,
    Rung::DevCheckout,
];

/// One bundled-asset resolution, with the rung that answered.
///
/// A struct rather than the `(Option<PathBuf>, Rung)` pair the plan sketched,
/// because [`rejected`](Self::rejected) does not fit in a pair and is the most
/// diagnostic field of the three: it is what separates *"the dev checkout
/// answered"* from *"the bundle was there, was rejected, and then the dev
/// checkout answered"*. Those look identical in the rung alone, and the second
/// is a bundle defect a published install would hit as `unresolved`. Widening
/// the pair rather than adding a second function keeps one shape for one
/// question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledResourceResolution {
    /// The path that answered, `None` when nothing did.
    pub path: Option<PathBuf>,
    /// Which rung answered, or [`Rung::Unresolved`] when every rung missed.
    /// Never [`Rung::Unknown`]: this function LOOKED, so an absence here is a
    /// finding about the machine and not about the observer.
    pub rung: Rung,
    /// Every higher-priority rung that did not answer, and why — rendered as
    /// `"<rung>: <reason>"`, joined by `"; "` in priority order. `None` only
    /// when the best rung answered outright.
    ///
    /// Carried **on success**, which is the entire reason it exists.
    pub rejected: Option<String>,
}

impl BundledResourceResolution {
    /// Build the report from a walk over [`CANDIDATE_RUNGS`]-ordered
    /// candidates.
    fn from_outcome(outcome: FirstExisting) -> Self {
        let rung = match outcome.index {
            Some(index) => CANDIDATE_RUNGS[index],
            None => Rung::Unresolved,
        };
        let rejected = if outcome.misses.is_empty() {
            None
        } else {
            Some(
                outcome
                    .misses
                    .iter()
                    .map(|(index, miss)| {
                        format!("{}: {}", CANDIDATE_RUNGS[*index].wire(), miss.describe())
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        BundledResourceResolution {
            path: outcome.path,
            rung,
            rejected,
        }
    }

    /// The `bundled_resources` row of the capability manifest.
    #[must_use]
    pub fn observation(&self) -> CapabilityObservation {
        let mut obs = CapabilityObservation::new(self.rung);
        if let Some(path) = &self.path {
            obs = obs.with_resolved_path(path.display().to_string());
        }
        if let Some(rejected) = &self.rejected {
            obs = obs.with_rejected(rejected.clone());
        }
        obs
    }
}

/// Resolve `relative` the way this module's callers do, and report WHICH RUNG
/// answered.
///
/// The reporting twin of `resolve(…)` + `dev_checkout(…)` + [`first_existing`],
/// walking the same three candidates in the same order — see
/// [`CANDIDATE_RUNGS`]. It is a pure observation: it opens nothing, writes
/// nothing, and changes no caller's behaviour.
///
/// # Why the workspace root comes from the READ-ONLY door
///
/// [`dev_checkout`] resolves it through
/// [`crate::workspace_paths::workspace_root`], which reads the setting via
/// `config_facade::get_setting` → `settings::load_settings_full` — and that door
/// runs the `claude-accounts.json` migration, can mint a `local_user_id` UUID
/// and `save_settings` the operator's real `settings.json`, and reaches the OS
/// keyring. That is fine for a runtime resolution, and disqualifying for a
/// DIAGNOSTIC: a manifest that mutates settings as a side effect of reporting
/// has changed the answer by asking the question. So this door uses
/// [`crate::workspace_paths::workspace_root_readonly`], which resolves the same
/// value from a non-mutating read. `dev_checkout` is untouched.
#[must_use]
pub fn resolve_with_rung(relative: &Path) -> BundledResourceResolution {
    let bundled = resolve(relative);
    let exe = std::env::current_exe().ok();
    let root = crate::workspace_paths::workspace_root_readonly();
    resolve_with_rung_in(
        bundled.as_deref(),
        exe.as_deref(),
        root.as_deref(),
        relative,
    )
}

/// Pure core of [`resolve_with_rung`] with every anchor injected, so the rung
/// mapping is unit-testable against a synthetic tree — no env read, no Tauri
/// runtime, no settings door, no dependency on how the machine running the suite
/// is laid out.
fn resolve_with_rung_in(
    bundled: Option<&Path>,
    exe: Option<&Path>,
    workspace_root: Option<&Path>,
    relative: &Path,
) -> BundledResourceResolution {
    let exe_relative = exe_relative_checkout(exe, relative);
    let dev = workspace_root_checkout(workspace_root, relative);
    BundledResourceResolution::from_outcome(first_existing_indexed([
        bundled,
        exe_relative.as_deref(),
        dev.as_deref(),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// pid/counter-scoped because this fleet runs `cargo test` from several
    /// worktrees at once; `Drop` cleanup so a failing assertion cannot leak the
    /// tree.
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture() -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "qontinui_bundled_resources_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p
    }

    /// Order is priority order, and the first EXISTING candidate wins — not the
    /// first present one.
    #[test]
    fn first_existing_returns_the_earliest_candidate_that_is_on_disk() {
        let f = fixture();
        let later = touch(&f.root, "later");
        let missing = f.root.join("missing");

        assert_eq!(
            first_existing([Some(missing.as_path()), Some(later.as_path())]),
            Some(later)
        );
    }

    /// A `None` candidate is skipped, not treated as the end of the list — this
    /// is what lets a machine with no `AppHandle` still reach the dev rung.
    #[test]
    fn a_none_candidate_is_skipped_rather_than_short_circuiting() {
        let f = fixture();
        let dev = touch(&f.root, "dev");

        assert_eq!(first_existing([None, Some(dev.as_path())]), Some(dev));
    }

    /// Nothing on disk means nothing resolved — never a guess at the first
    /// candidate.
    #[test]
    fn no_existing_candidate_resolves_to_none() {
        let f = fixture();
        let a = f.root.join("a");
        let b = f.root.join("b");

        assert_eq!(first_existing([Some(a.as_path()), Some(b.as_path())]), None);
    }

    /// An empty candidate list is `None`, not a panic — the const-generic N=0
    /// instantiation has to be well-behaved because callers build their arrays
    /// from optional rungs.
    #[test]
    fn an_empty_candidate_list_resolves_to_none() {
        let empty: [Option<&Path>; 0] = [];
        assert_eq!(first_existing(empty), None);
    }

    // -----------------------------------------------------------------
    // dev_checkout — the exe-relative rung outranks the workspace root
    // -----------------------------------------------------------------

    /// A relative asset path standing in for a real bundled resource. Nested,
    /// so the test exercises the same join shape the callers use.
    fn asset_relpath() -> PathBuf {
        Path::new("resources")
            .join("code-semantics")
            .join("ts-language-service.mjs")
    }

    /// Lay out `<base>/qontinui-runner/src-tauri/<relative>` and return the
    /// path to the asset.
    fn checkout_with_asset(base: &Path, relative: &Path) -> PathBuf {
        let asset = base.join(RUNNER_REPO_DIR).join("src-tauri").join(relative);
        std::fs::create_dir_all(asset.parent().unwrap()).unwrap();
        std::fs::write(&asset, b"x").unwrap();
        asset
    }

    /// The dev-build exe location inside a checkout laid out by
    /// [`checkout_with_asset`].
    fn dev_exe_in(base: &Path) -> PathBuf {
        base.join(RUNNER_REPO_DIR)
            .join("src-tauri")
            .join("target")
            .join("debug")
            .join("qontinui-runner.exe")
    }

    /// **The fleet's normal dev loop.** A temp runner built in an agent
    /// worktree must load the worktree's assets, not the canonical checkout's.
    /// The configured `paths.workspace_root` setting outranks the exe anchor
    /// inside `qontinui_types::paths`, so without the exe-relative rung the
    /// worktree binary silently exercises a copy the operator did not edit —
    /// invisible, and possibly hundreds of commits stale.
    #[test]
    fn the_exe_relative_copy_wins_over_the_workspace_root_copy() {
        let f = fixture();
        let relative = asset_relpath();

        let canonical = f.root.join("canonical");
        let canonical_asset = checkout_with_asset(&canonical, &relative);
        let worktree = f.root.join("_wt").join("some-tag");
        let worktree_asset = checkout_with_asset(&worktree, &relative);
        let exe = dev_exe_in(&worktree);

        assert_eq!(
            dev_checkout_in(Some(&exe), Some(&canonical), &relative),
            Some(worktree_asset),
            "the binary must load the assets of the tree it was built in"
        );
        // …and the canonical copy is genuinely there, so this is a preference
        // between two existing files rather than the only answer available.
        assert!(canonical_asset.is_file());
    }

    /// An installed binary has no useful ancestry, so the workspace-root rung
    /// still answers — that is why it is kept rather than replaced.
    #[test]
    fn an_installed_exe_falls_through_to_the_workspace_root_rung() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        let canonical_asset = checkout_with_asset(&canonical, &relative);
        let installed_exe = f.root.join("program-files").join("qontinui-runner.exe");

        assert_eq!(
            dev_checkout_in(Some(&installed_exe), Some(&canonical), &relative),
            Some(canonical_asset)
        );
    }

    /// The workspace-root rung is deliberately NOT existence-checked here (the
    /// caller's [`first_existing`] owns that), but the exe-relative rung is —
    /// an exe whose own tree lacks the asset must not shadow the next rung.
    #[test]
    fn the_exe_rung_is_existence_checked_and_does_not_shadow_the_next() {
        let f = fixture();
        let relative = asset_relpath();

        // A dev-shaped exe in a checkout that does NOT carry the asset.
        let bare = f.root.join("bare");
        std::fs::create_dir_all(bare.join(RUNNER_REPO_DIR).join("src-tauri")).unwrap();
        let exe = dev_exe_in(&bare);

        let canonical = f.root.join("canonical");
        let canonical_asset = checkout_with_asset(&canonical, &relative);

        assert_eq!(
            dev_checkout_in(Some(&exe), Some(&canonical), &relative),
            Some(canonical_asset)
        );
    }

    /// Fail-soft on every input: no exe and no root is `None`, never a
    /// cwd-relative guess. A cwd answer would make the asset a function of how
    /// the process was launched — the defect this plan exists to remove.
    #[test]
    fn dev_checkout_degrades_to_none_with_no_anchors() {
        assert_eq!(dev_checkout_in(None, None, &asset_relpath()), None);
    }

    // -----------------------------------------------------------------
    // Phase 2 of `2026-08-31-published-build-parity-check` — which rung
    // answered, and what it stepped over.
    // -----------------------------------------------------------------

    /// The index the existence walk reports is the winner's, and the candidates
    /// it stepped over are recorded rather than discarded — on a SUCCESSFUL
    /// resolution.
    #[test]
    fn first_existing_indexed_reports_the_winning_index_and_the_misses_before_it() {
        let f = fixture();
        let missing = f.root.join("missing");
        let winner = touch(&f.root, "winner");
        let later = touch(&f.root, "later");

        let got = first_existing_indexed([
            None,
            Some(missing.as_path()),
            Some(winner.as_path()),
            Some(later.as_path()),
        ]);

        assert_eq!(got.path.as_deref(), Some(winner.as_path()));
        assert_eq!(got.index, Some(2));
        assert_eq!(
            got.misses,
            vec![
                (0, CandidateMiss::Absent),
                (1, CandidateMiss::NotOnDisk(missing)),
            ],
            "candidates AFTER the winner are not misses — they were never tried"
        );
    }

    /// The indexed walk is the same walk: [`first_existing`] delegates to it, so
    /// the two can never disagree about which file answers.
    #[test]
    fn first_existing_delegates_to_the_indexed_walk() {
        let f = fixture();
        let missing = f.root.join("missing");
        let winner = touch(&f.root, "winner");

        let candidates = [None, Some(missing.as_path()), Some(winner.as_path())];
        assert_eq!(
            first_existing(candidates),
            first_existing_indexed(candidates).path
        );
    }

    /// Nothing on disk means no index at all — never a guess at candidate 0.
    #[test]
    fn first_existing_indexed_reports_no_index_when_nothing_answers() {
        let f = fixture();
        let a = f.root.join("a");

        let got = first_existing_indexed([Some(a.as_path()), None]);

        assert_eq!(got.path, None);
        assert_eq!(got.index, None);
        assert_eq!(
            got.misses,
            vec![(0, CandidateMiss::NotOnDisk(a)), (1, CandidateMiss::Absent)],
            "with no winner every candidate is a miss, so the walk is fully described"
        );
    }

    /// The bundle rung is the top rung, and a published install is expected to
    /// answer here — a `bundle_resource` reading on a dev box and on an
    /// operator's box is the same reading, which is the point of the rung.
    #[test]
    fn the_bundle_rung_is_reported_when_the_unpacked_resource_answers() {
        let f = fixture();
        let relative = asset_relpath();
        let bundle_dir = f.root.join("install").join("resources");
        std::fs::create_dir_all(bundle_dir.join(relative.parent().unwrap())).unwrap();
        let bundled = bundle_dir.join(&relative);
        std::fs::write(&bundled, b"x").unwrap();

        // A dev checkout is ALSO present, so this is a preference between two
        // existing files rather than the only answer available.
        let canonical = f.root.join("canonical");
        checkout_with_asset(&canonical, &relative);

        let got = resolve_with_rung_in(
            Some(&bundled),
            Some(&dev_exe_in(&canonical)),
            Some(&canonical),
            &relative,
        );

        assert_eq!(got.rung, Rung::BundleResource);
        assert_eq!(got.path.as_deref(), Some(bundled.as_path()));
        assert_eq!(
            got.rejected, None,
            "the best rung answered outright, so nothing was stepped over"
        );
    }

    /// The exe-relative rung is reported apart from the workspace-root one, even
    /// though `dev_checkout` returns them as a single `Option<PathBuf>`. In this
    /// fleet's normal dev loop these two are DIFFERENT checkouts, so collapsing
    /// them would erase the distinction the module exists to make.
    #[test]
    fn the_exe_relative_rung_is_reported_separately_from_the_workspace_root_rung() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        checkout_with_asset(&canonical, &relative);
        let worktree = f.root.join("_wt").join("some-tag");
        let worktree_asset = checkout_with_asset(&worktree, &relative);

        let got = resolve_with_rung_in(
            None,
            Some(&dev_exe_in(&worktree)),
            Some(&canonical),
            &relative,
        );

        assert_eq!(got.rung, Rung::ExeRelativeCheckout);
        assert_eq!(got.path.as_deref(), Some(worktree_asset.as_path()));
    }

    /// **The rejection survives a successful resolution.** An installed binary
    /// with no useful ancestry falls to the workspace-root rung, and the row
    /// says so — reporting only `dev_checkout` would hide that the bundle rung
    /// was never even offered, which on a published install is the defect.
    #[test]
    fn the_workspace_root_rung_answers_and_the_skipped_rungs_are_still_reported() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        let canonical_asset = checkout_with_asset(&canonical, &relative);
        let installed_exe = f.root.join("program-files").join("qontinui-runner.exe");

        let got = resolve_with_rung_in(None, Some(&installed_exe), Some(&canonical), &relative);

        assert_eq!(got.rung, Rung::DevCheckout);
        assert_eq!(got.path.as_deref(), Some(canonical_asset.as_path()));
        let rejected = got
            .rejected
            .as_deref()
            .expect("two rungs were stepped over, so the fall-through must be reported");
        assert!(
            rejected.starts_with("bundle_resource: "),
            "the highest-priority miss comes first: {rejected}"
        );
        assert!(
            rejected.contains("exe_relative_checkout: "),
            "no miss is dropped for being second: {rejected}"
        );
    }

    /// A bundle rung that NAMED a path which is not on disk is a different
    /// finding from one that offered nothing — the first is a bundle defect
    /// (an installer that ships a path it does not carry), the second is just
    /// a process with no `AppHandle`.
    #[test]
    fn a_present_but_missing_bundle_candidate_is_reported_as_not_on_disk() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        checkout_with_asset(&canonical, &relative);
        let uninstalled = f.root.join("install").join("resources").join(&relative);

        let got = resolve_with_rung_in(
            Some(&uninstalled),
            Some(&dev_exe_in(&canonical)),
            Some(&canonical),
            &relative,
        );

        assert_eq!(got.rung, Rung::ExeRelativeCheckout);
        let rejected = got.rejected.as_deref().expect("the bundle rung was tried");
        assert!(
            rejected.contains("is not on disk"),
            "a bundle that named a path it does not carry must not read as \
             'offered no candidate': {rejected}"
        );
    }

    /// Every rung missed is `unresolved` — a stated finding about the machine.
    /// It must never be `unknown` (a finding about the observer) and never a
    /// guess at the rung the code would have used.
    #[test]
    fn nothing_anywhere_is_reported_as_unresolved_rather_than_guessed() {
        let got = resolve_with_rung_in(None, None, None, &asset_relpath());

        assert_eq!(got.rung, Rung::Unresolved);
        assert_ne!(
            got.rung,
            Rung::Unknown,
            "this function LOOKED — an absence here is about the machine"
        );
        assert_eq!(got.path, None);
        let rejected = got
            .rejected
            .as_deref()
            .expect("every rung missed, so every rung is named");
        for rung in CANDIDATE_RUNGS {
            assert!(
                rejected.contains(rung.wire()),
                "{} must appear in the rejection: {rejected}",
                rung.wire()
            );
        }
    }

    /// The reporting walk and the shipped walk agree on the PATH for every
    /// input the callers use. `resolve_with_rung` is an observation of the
    /// existing resolution, not a second one — if these ever disagree, the
    /// manifest is describing a resolution that does not happen.
    #[test]
    fn the_rung_report_never_disagrees_with_dev_checkout_about_the_path() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        checkout_with_asset(&canonical, &relative);
        let worktree = f.root.join("_wt").join("some-tag");
        checkout_with_asset(&worktree, &relative);
        let bare = f.root.join("bare");
        std::fs::create_dir_all(bare.join(RUNNER_REPO_DIR).join("src-tauri")).unwrap();
        let installed_exe = f.root.join("program-files").join("qontinui-runner.exe");

        for (exe, root) in [
            (Some(dev_exe_in(&worktree)), Some(canonical.clone())),
            (Some(dev_exe_in(&bare)), Some(canonical.clone())),
            (Some(installed_exe.clone()), Some(canonical.clone())),
            (Some(installed_exe.clone()), None),
            (None, Some(canonical.clone())),
            (None, None),
        ] {
            let shipped = dev_checkout_in(exe.as_deref(), root.as_deref(), &relative);
            let reported = resolve_with_rung_in(None, exe.as_deref(), root.as_deref(), &relative);
            // The shipped walk is not existence-checked on its last rung; the
            // report is (that check lives in `first_existing` for both). So the
            // agreement asserted is the one that matters: whenever the report
            // resolves, it resolves to the SAME path the callers would use.
            if let Some(path) = &reported.path {
                assert_eq!(
                    shipped.as_deref(),
                    Some(path.as_path()),
                    "the report and the shipped resolution must not diverge \
                     (exe={exe:?}, root={root:?})"
                );
            }
        }
    }

    /// The observation carries all three of the resolution's fields into the
    /// manifest row, rejection included.
    #[test]
    fn the_observation_carries_the_rung_the_path_and_the_rejection() {
        let f = fixture();
        let relative = asset_relpath();
        let canonical = f.root.join("canonical");
        let canonical_asset = checkout_with_asset(&canonical, &relative);
        let installed_exe = f.root.join("program-files").join("qontinui-runner.exe");

        let obs = resolve_with_rung_in(None, Some(&installed_exe), Some(&canonical), &relative)
            .observation();

        assert_eq!(obs.rung, Rung::DevCheckout);
        assert_eq!(
            obs.resolved_path.as_deref(),
            Some(canonical_asset.display().to_string().as_str())
        );
        assert!(
            obs.rejected.is_some(),
            "a fall-through that is not shown is a fall-through nobody knows happened"
        );
    }

    /// **The guard that makes this whole module's premise falsifiable.**
    ///
    /// [`resolve`] is only correct if `bundle.resources` actually ships the
    /// files it names. The dangerous failure is silent: a glob that matches
    /// nothing is a perfectly valid config and a perfectly empty install, and
    /// every caller then falls through to the dev rung — so the bug is
    /// invisible on the author's machine, which is precisely the class of
    /// defect this plan exists to kill. `cargo check` cannot see it and neither
    /// can a config-key assertion.
    ///
    /// Reading the manifest through `env!("CARGO_MANIFEST_DIR")` is correct
    /// HERE and nowhere else in this module: under `#[cfg(test)]` the source
    /// tree the binary was built from IS the subject under test (the plan's
    /// class C — the same reason `build_drift.rs` bakes it deliberately).
    #[test]
    fn every_declared_bundle_resource_glob_matches_at_least_one_file() {
        let tauri_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tauri_dir.join("tauri.conf.json")).unwrap(),
        )
        .unwrap();

        let declared = conf["bundle"]["resources"]
            .as_array()
            .expect("bundle.resources must be a list — the runner declares the list form, and the map form would change resource_relpath semantics");
        assert!(
            !declared.is_empty(),
            "bundle.resources is empty: nothing ships, and every bundled-asset lookup silently degrades to the dev rung"
        );

        for entry in declared {
            let pattern = entry
                .as_str()
                .expect("each resource entry is a glob string");
            // Globs are relative to the tauri dir, which is also what
            // `resource_relpath` keys the unpacked layout on.
            let absolute = tauri_dir.join(pattern);
            let matched = glob::glob(&absolute.to_string_lossy())
                .expect("resource glob must be well-formed")
                .filter_map(Result::ok)
                .any(|p| p.is_file());
            assert!(
                matched,
                "bundle.resources glob {pattern:?} matches no file on disk. \
                 A glob that matches nothing is a green config and an empty install: \
                 the bundled rung would resolve a path present on no host, and every \
                 caller would silently fall through to the dev checkout."
            );
        }
    }

    /// Every asset this module was introduced for is covered by the declared
    /// globs. Named explicitly so that deleting a glob, or narrowing one, or
    /// moving an asset out from under one, fails here rather than at runtime on
    /// a user's install.
    ///
    /// `data/htn_methods` is a DIRECTORY (the `methods_directory` rung hands
    /// Python the dir, not a file), so it is required as a directory whose
    /// contents a glob must reach — a check a file-only list would have left
    /// the narrowing of `resources/**/*` free to break by accident.
    #[test]
    fn the_helper_script_and_htn_data_are_both_covered_by_a_declared_glob() {
        let tauri_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tauri_dir.join("tauri.conf.json")).unwrap(),
        )
        .unwrap();
        let declared: Vec<String> = conf["bundle"]["resources"]
            .as_array()
            .expect("bundle.resources must be a list")
            .iter()
            .map(|e| {
                tauri_dir
                    .join(e.as_str().unwrap())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        // (path, is_directory)
        for (required, is_dir) in [
            ("resources/code-semantics/ts-language-service.mjs", false),
            ("data/runner_state_machine.json", false),
            ("data/htn_methods", true),
        ] {
            let target = tauri_dir.join(required);
            if is_dir {
                assert!(
                    target.is_dir(),
                    "{required} is missing from the checkout — the `methods_directory` rung has \
                     nothing to ship"
                );
            } else {
                assert!(
                    target.is_file(),
                    "{required} is missing from the checkout — the bundled-asset rung has nothing to ship"
                );
            }

            // A directory is "covered" when a glob reaches something INSIDE it:
            // an installer that created an empty directory would satisfy the
            // `methods_directory` existence check and still give the planner no
            // methods at all.
            let covered = declared.iter().any(|pattern| {
                glob::glob(pattern)
                    .expect("resource glob must be well-formed")
                    .filter_map(Result::ok)
                    .any(|p| {
                        if is_dir {
                            p.is_file() && p.starts_with(&target)
                        } else {
                            p == target
                        }
                    })
            });
            assert!(
                covered,
                "{required} exists but no bundle.resources glob reaches it, so it ships in no installer"
            );
        }
    }
}
