//! The runner's single door to "where do the Qontinui repo checkouts live?".
//!
//! Plan: `2026-08-04-remove-hardcoded-machine-paths-from-product-code.md`
//! (slice 1). This module is Phase 0 of that slice: it adds the
//! `paths.workspace_root` setting and the one-time migration that persists
//! **whatever this machine already resolves to**, so the later phases can
//! delete the hardcoded `D:/qontinui-root` fallbacks without taking an existing
//! install down with them.
//!
//! ## Why the bridge is load-bearing, not belt-and-braces
//!
//! Four modules (`agent_worktree/census.rs`, `fleet.rs`, `coord_mcp.rs`,
//! `agent_runtime.rs`) each carried a byte-similar resolver whose Windows arm
//! was a hardcoded `D:/qontinui-root`. On the author's own machine
//! `QONTINUI_ROOT` is unset and `$HOME/qontinui-root` does not exist — so that
//! literal is not a dead fallback, it is the **live** resolution path. Deleting
//! it with nothing recorded in its place makes the census blind, the fleet
//! publisher silent, the `.mcp.json` reconcile inert and canonical-checkout
//! creation fail. That is an outage, not a refactor.
//!
//! So the removal ships in two steps, in this order:
//!
//! 1. **(here, Phase 0)** persist the currently-resolved root into
//!    `settings.json`. Nothing is deleted yet, and the value comes from the
//!    machine's own live resolution — never from a literal in this file. That
//!    resolution deliberately includes the **legacy** resolver as a last resort
//!    (see [`root_to_bridge`]): on an installed binary the literal is the only
//!    thing answering today, and a bridge that skipped it would record nothing
//!    in exactly the case it exists to protect.
//! 2. **(later phases)** delete the literals; the resolver then reads the
//!    persisted setting.
//!
//! The resolution itself lives in `qontinui_types::paths`, shared with
//! qontinui-coord, so there is exactly one answer to this question across the
//! Rust stack.
//!
//! ## Startup ordering
//!
//! [`persist_resolved_workspace_root`] runs *after* the background thread that
//! spawns the census, the fleet heartbeat and the agent runtime — i.e. after
//! every consumer of the root has started. That is harmless while those
//! consumers resolve the root themselves (they reach the same answer with or
//! without the setting), and it is why the setting is a *bridge* rather than a
//! dependency: **no consumer may require the persisted value to be present
//! within the first boot.** A consumer that ever needs that guarantee must move
//! this call above the thread spawn in `main.rs` first.

use std::path::{Path, PathBuf};

use qontinui_types::paths::{qontinui_workspace_root, WorkspaceAnchor, WorkspaceRoot};
use tracing::{info, warn};

use crate::config_facade::{get_setting, update_setting};
use crate::settings::PathSettings;

/// This repo's checkout directory name — the anchor predicate looks for
/// `<candidate>/qontinui-runner/.git`.
const RUNNER_REPO_DIR: &str = "qontinui-runner";

/// Resolve the workspace root for this runner process.
///
/// Delegates to the shared [`qontinui_workspace_root`], supplying the two
/// things only the runner can know:
///
/// - `configured` — the `paths.workspace_root` setting (Phase 0's bridge).
/// - `anchor` — the running executable, so a **dev install** running out of a
///   checkout (`<root>/qontinui-runner/src-tauri/target/…/qontinui-runner.exe`)
///   resolves the root with zero configuration by walking up to the directory
///   containing `qontinui-runner/.git`.
///
/// The anchor is passed **in** rather than taken inside the helper on purpose:
/// an installed binary under `Program Files` has no useful ancestry, and
/// `current_exe()` resolves through symlinks. Supplying it is the caller's
/// judgement that its ancestry is meaningful; the helper never assumes it.
/// A failure to read `current_exe()` degrades to "no anchor", never to a guess.
///
/// Returns the full [`WorkspaceRoot`] so each caller picks its own disposition:
/// discovery surfaces take [`WorkspaceRoot::into_root`] and degrade, surfaces
/// that write or execute take [`WorkspaceRoot::require`] and fail closed with an
/// error naming `$QONTINUI_ROOT`.
///
/// **Not memoized, and not yet on a hot path.** Each call re-reads the settings
/// file. Phase 0 calls it once, at startup. Before a later phase puts it behind
/// the census or fleet pollers, resolve it once into a `OnceLock` (or pass the
/// resolved root down) — `qontinui_types::paths` states that the runner should
/// resolve this once, not per call site.
pub fn runner_workspace_root() -> WorkspaceRoot {
    let configured = get_setting::<PathSettings>().workspace_root;
    let exe = std::env::current_exe().ok();
    qontinui_workspace_root(configured.as_deref(), exe_anchor(exe.as_deref()))
}

/// The runner's contribution to the resolution: this executable's path, tagged
/// with **this** repo's directory name so the ancestor-walk predicate looks for
/// `<candidate>/qontinui-runner/.git`.
///
/// Split out as a pure function because it is the only part of the resolution
/// the runner owns — the ordering and the probes belong to
/// `qontinui_types::paths` and are tested there, against injected inputs. Testing
/// them again through this module would mean reading the ambient environment,
/// which makes a test's verdict depend on the machine it runs on.
fn exe_anchor(exe: Option<&Path>) -> Option<WorkspaceAnchor<'_>> {
    exe.map(|e| WorkspaceAnchor::new(e, RUNNER_REPO_DIR))
}

/// What, if anything, the migration should write. Pure — no IO, no env — so the
/// idempotence and non-destructiveness rules are unit-testable directly.
///
/// - An `existing` value that is present and non-blank means the operator (or an
///   earlier run) already answered; **never** overwrite it.
/// - Otherwise write `resolved`, if anything resolved at all.
/// - A blank existing value is *unset*, not a directory named `""`, so it is
///   replaceable.
fn migration_write(existing: Option<&str>, resolved: Option<&Path>) -> Option<String> {
    if existing.is_some_and(|s| !s.trim().is_empty()) {
        return None;
    }
    resolved.map(|p| p.to_string_lossy().into_owned())
}

/// The root to bridge: what this machine resolves to **today**, counting the
/// resolution that is still live in this build.
///
/// The new resolver is tried first. When it misses, the **legacy** resolver is
/// consulted — the one whose Windows arm is still the hardcoded literal at the
/// time Phase 0 ships.
///
/// **This fallback is the entire point of Phase 0 and is not redundant.** On a
/// *dev* install the new resolver already succeeds via the executable-ancestor
/// walk, so the bridge is belt-and-braces there. On an **installed binary**
/// (`Program Files`, no useful ancestry) with `$QONTINUI_ROOT` unset and no
/// `$HOME/qontinui-root`, the new resolver returns `None` and the literal is the
/// only thing answering — which is precisely the install shape that would take
/// the outage when the literal is deleted. Seeding from the legacy resolver is
/// what captures that answer before it disappears.
///
/// Deleted together with the last legacy copy, once every consumer reads the
/// setting.
fn root_to_bridge() -> Option<PathBuf> {
    let resolved = runner_workspace_root();
    if let Some(rejected) = resolved.rejected {
        warn!(
            "workspace_paths: {} — continuing with the next resolution rung",
            rejected.describe()
        );
    }
    resolved
        .into_root()
        .or_else(crate::agent_worktree::census::qontinui_root)
}

/// One-time bridge: record the workspace root this machine **already** resolves
/// to, so the hardcoded fallbacks can be deleted without changing behaviour on
/// an existing install.
///
/// Idempotent and non-destructive:
///
/// - a `paths.workspace_root` that is already set is **never** overwritten —
///   the operator's configuration always wins over this migration;
/// - when nothing resolves there is nothing to bridge, so it writes nothing and
///   says so rather than inventing a path;
/// - the value written comes from the machine's own live resolution. This file
///   contains no path literal of its own.
///
/// **Primary only.** A secondary launched with `QONTINUI_INSTANCE_NAME` but no
/// `QONTINUI_CONFIG_DIR` resolves the *primary's* shared `settings.json`
/// (`settings.rs`'s FOOTGUN GUARD), and this migration is first-writer-wins, so
/// letting a secondary run it would let whichever instance booted first freeze
/// the value. Same rule as `settings::should_persist_migration`.
///
/// Called once at startup. Failures are non-fatal: a runner that cannot write
/// its settings still boots, and the resolver still has its other rungs.
/// `update_settings` refuses to write over a non-authoritative base, so a
/// corrupt `settings.json` yields `Err` here rather than being clobbered.
pub fn persist_resolved_workspace_root() -> Result<(), String> {
    if crate::instance::is_secondary() {
        return Ok(());
    }

    let existing = get_setting::<PathSettings>().workspace_root;
    if existing.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        return Ok(());
    }

    let Some(value) = migration_write(existing.as_deref(), root_to_bridge().as_deref()) else {
        info!(
            "workspace_paths: no workspace root resolves on this machine, so \
             `paths.workspace_root` was left unset. Set $QONTINUI_ROOT (or the \
             setting) if this runner should manage canonical checkouts."
        );
        return Ok(());
    };

    update_setting::<PathSettings, _>(|paths| paths.workspace_root = Some(value.clone()))?;
    info!("workspace_paths: recorded paths.workspace_root = {value:?} from this machine's live resolution");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qontinui_types::paths::resolve_workspace_root;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A synthetic workspace laid out like a **dev install**: the runner exe
    /// under `<root>/qontinui-runner/src-tauri/target/release/`, with the repo's
    /// `.git` present so the anchor predicate can fire.
    ///
    /// Never the real machine layout, and pid/counter-scoped because this fleet
    /// runs `cargo test` from several worktrees at once. Cleanup is a `Drop`
    /// guard so a failing assertion does not leak the tree.
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
            "qontinui_workspace_paths_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(
            root.join(RUNNER_REPO_DIR)
                .join("src-tauri")
                .join("target")
                .join("release"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join(RUNNER_REPO_DIR).join(".git")).unwrap();
        Fixture { root }
    }

    fn dev_install_exe(root: &Path) -> PathBuf {
        root.join(RUNNER_REPO_DIR)
            .join("src-tauri")
            .join("target")
            .join("release")
            .join("qontinui-runner.exe")
    }

    /// The anchor names THIS repo, so the predicate looks for
    /// `<candidate>/qontinui-runner/.git` and not some other repo's marker.
    #[test]
    fn exe_anchor_tags_the_executable_with_this_repos_directory_name() {
        let exe = PathBuf::from("/anywhere/qontinui-runner.exe");
        let anchor = exe_anchor(Some(&exe)).expect("an exe must produce an anchor");
        assert_eq!(anchor.start, exe.as_path());
        assert_eq!(anchor.repo_name, "qontinui-runner");
    }

    /// A runner that cannot read its own executable path degrades to "no
    /// anchor" — it must never fall back to the cwd or to a guess.
    #[test]
    fn a_missing_executable_path_yields_no_anchor() {
        assert!(exe_anchor(None).is_none());
    }

    /// The zero-configuration dev-install path, end to end through the real
    /// resolver: the exe lives inside the checkout, and the walk finds `<root>`
    /// because it holds `qontinui-runner/.git`.
    ///
    /// Every input to `resolve_workspace_root` is injected here — no env read —
    /// so the verdict does not depend on the machine the suite runs on.
    #[test]
    fn the_exe_anchor_resolves_a_dev_install_with_no_configuration() {
        let f = fixture();
        let exe = dev_install_exe(&f.root);

        let got = resolve_workspace_root(None, None, None, exe_anchor(Some(&exe)), None);

        assert_eq!(
            got.root.as_deref(),
            Some(f.root.as_path()),
            "the exe-ancestor walk must find the workspace root with no configuration"
        );
    }

    /// The Phase 0 bridge, end to end: an INSTALLED binary has no useful
    /// ancestry, so the persisted `paths.workspace_root` setting is what keeps
    /// resolution working once the hardcoded fallbacks are gone.
    #[test]
    fn the_persisted_setting_resolves_an_installed_binary_with_useless_ancestry() {
        let f = fixture();
        let installed_exe = std::env::temp_dir()
            .join("qontinui_no_such_install_dir")
            .join("qontinui-runner.exe");

        let got = resolve_workspace_root(
            None,
            None,
            f.root.to_str(),
            exe_anchor(Some(&installed_exe)),
            None,
        );

        assert_eq!(got.root.as_deref(), Some(f.root.as_path()));
        assert_eq!(got.rejected, None);
    }

    // -----------------------------------------------------------------
    // The migration's decision rule. Pure, so idempotence and
    // non-destructiveness are asserted directly rather than inferred.
    // -----------------------------------------------------------------

    /// The operator's configuration always wins: an already-set value is never
    /// overwritten, however the machine resolves today. This is what makes the
    /// migration safe to run on every boot.
    #[test]
    fn migration_never_overwrites_an_existing_value() {
        assert_eq!(
            migration_write(Some("D:/operator/choice"), Some(Path::new("D:/resolved"))),
            None
        );
        assert_eq!(migration_write(Some("D:/operator/choice"), None), None);
    }

    /// A blank setting is *unset*, not a directory named `""` — so it is
    /// replaceable rather than being treated as an operator decision.
    #[test]
    fn migration_treats_a_blank_existing_value_as_unset() {
        assert_eq!(
            migration_write(Some("   "), Some(Path::new("D:/resolved"))),
            Some("D:/resolved".to_string())
        );
    }

    /// Nothing resolved means nothing to bridge. The migration must write
    /// nothing rather than invent a path — an unresolved root is UNKNOWN, not a
    /// guess.
    #[test]
    fn migration_writes_nothing_when_nothing_resolves() {
        assert_eq!(migration_write(None, None), None);
        assert_eq!(migration_write(Some(""), None), None);
    }

    /// The bridge case: unset setting plus a resolved root records it verbatim.
    #[test]
    fn migration_records_the_resolved_root_when_unset() {
        assert_eq!(
            migration_write(None, Some(Path::new("D:/qontinui-root"))),
            Some("D:/qontinui-root".to_string())
        );
    }
}
