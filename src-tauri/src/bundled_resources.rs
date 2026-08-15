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
    exe_relative_checkout(exe, relative).or_else(|| {
        workspace_root.map(|root| root.join(RUNNER_REPO_DIR).join("src-tauri").join(relative))
    })
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
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .map(Path::to_path_buf)
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
