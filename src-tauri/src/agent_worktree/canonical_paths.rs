//! Canonical-checkout path resolution for the runner's flat worktree
//! layout (`<root>/<name>/`).
//!
//! Plan: `2026-05-29-runner-canonical-path-slug-normalization.md`.
//!
//! Coord stores repos as `owner/name` slugs (e.g. `qontinui/qontinui-runner`,
//! enforced at `qontinui-coord/src/canonical_repos.rs`), but the runner's
//! on-disk layout is owner-flat — the canonical checkout for that repo lives
//! at `D:/qontinui-root/qontinui-runner/`, not `.../qontinui/qontinui-runner/`.
//! [`canonical_segment`] reconciles the two: it reduces any slug shape (bare,
//! `owner/name`, or a git URL) to the bare repo name used as the single
//! directory segment, so callers no longer need per-callsite
//! `repo_canonical_paths` overrides for the standard layout.

use std::path::{Path, PathBuf};

use qontinui_runner_lib::observable_bridge::git_ops::repo_basename_from_url;

/// Reduce a coord repo slug (or git URL) to the bare repo name used as a
/// directory segment in the runner's flat canonical layout
/// (`<root>/<name>/`).
///
/// Accepts: `qontinui-runner`, `qontinui/qontinui-runner`,
/// `git@github.com:qontinui/qontinui-runner.git`,
/// `https://github.com/qontinui/qontinui-runner.git`. Returns the bare
/// `qontinui-runner` for all of them. Rejects empty / whitespace-only /
/// trailing-slash inputs (`Err` so the caller surfaces a clear validation
/// error instead of silently building `<root>/`).
///
/// The actual reduction is delegated to `repo_basename_from_url`
/// (`qontinui_runner_lib::observable_bridge::git_ops`) — it already
/// splits on both `/` and `:`, strips a `.git` suffix, and so handles the SSH
/// `git@github.com:owner/name.git` form that a naive `rsplit_once('/')` would
/// mishandle (returning `name.git` from the colon-delimited shape). This
/// wrapper adds only the fail-closed validation around it.
pub fn canonical_segment(repo_slug: &str) -> Result<String, String> {
    let trimmed = repo_slug.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "repo slug {repo_slug:?} is empty or whitespace-only"
        ));
    }
    // A trailing slash means the name segment is missing (e.g. `qontinui/`
    // is owner-present, name-absent). `repo_basename_from_url` would silently
    // trim it and reinterpret the owner as the name; fail closed instead.
    if trimmed.ends_with('/') {
        return Err(format!(
            "repo slug {repo_slug:?} has an empty name segment (trailing slash)"
        ));
    }
    let segment = repo_basename_from_url(trimmed);
    if segment.is_empty() {
        return Err(format!(
            "repo slug {repo_slug:?} reduced to an empty path segment"
        ));
    }
    Ok(segment)
}

/// Build the default canonical-checkout path for `repo` on this host:
/// `<workspace-root>/<name>/`. The slug is normalized via
/// [`canonical_segment`], so both `qontinui-runner` and
/// `qontinui/qontinui-runner` resolve to the same on-disk location.
///
/// **One arm, not two.** This used to be a pair of `#[cfg]` functions: the
/// Windows arm hardcoded `D:/qontinui-root` and the POSIX arm read `$HOME` with
/// an `unwrap_or("/tmp")`. Both are gone (plan
/// `2026-08-04-remove-hardcoded-machine-paths-from-product-code`, slice 1
/// Phase 3) — the first because a shipped binary must not carry the author's
/// drive layout, the second because materializing a canonical checkout under
/// `/tmp` is the same silent-wrong-place bug with a friendlier face. The
/// per-platform behaviour they encoded now lives once, in
/// `qontinui_types::paths`, which probes `<home>/qontinui-root` on every OS.
///
/// **Fails closed.** This surface *creates* git checkouts, so an unresolved root
/// is an error naming `$QONTINUI_ROOT`, never a guess — `WorkspaceRoot::require`
/// rather than `into_root`. The signature was already `Result`, so every caller
/// already handles the arm; what changes is that on POSIX it can now actually be
/// taken instead of silently resolving to `/tmp`.
pub fn default_canonical_path(repo: &str) -> Result<PathBuf, String> {
    // Validate the slug BEFORE resolving the root: a malformed slug is a caller
    // bug that holds on every machine, and answering it with "cannot resolve the
    // Qontinui workspace root" would name the wrong thing entirely.
    let segment = canonical_segment(repo)?;
    let root = crate::workspace_paths::runner_workspace_root().require()?;
    Ok(root.join(segment))
}

/// Pure core of [`default_canonical_path`]: the workspace root is injected, so
/// the layout rule is unit-testable against a synthetic root with no dependency
/// on the machine the suite runs on. Same wrapper/core split as
/// [`agent_worktree_root`] / [`agent_worktree_root_inner`] below.
fn default_canonical_path_in(root: &Path, repo: &str) -> Result<PathBuf, String> {
    Ok(root.join(canonical_segment(repo)?))
}

/// Resolve the external root under which all agent worktrees for `canonical`
/// live. Order: explicit env override (absolute only) -> sibling of the
/// project root.
///
/// Pre-relocation the runner materialized worktrees INSIDE the repo checkout
/// (`<canonical>/agent-worktrees/<id>/<repo>`), which caused Windows watcher
/// deletion-locks, dirty-tree pollution, and editor noise. This resolver
/// moves them OUTSIDE the project tree — by default a sibling directory
/// `qontinui-worktrees` next to the project root (e.g.
/// `D:/qontinui-root/qontinui-coord` -> `D:/qontinui-root/qontinui-worktrees`).
///
/// The final worktree path for one repo is
/// `agent_worktree_root(canonical).join(agent_id).join(repo_name)`.
pub fn agent_worktree_root(canonical: &Path) -> PathBuf {
    // Honor BOTH knobs: the new runner-owned QONTINUI_WORKTREE_ROOT first, then
    // fall back to the EXISTING COORD_WORKTREE_ROOT so the documented escape
    // hatch is not silently broken. Only an *absolute* override is honored — a
    // relative value would reintroduce the ambient-cwd bug that the resolver
    // (and the old local_worktree_target) exists to prevent.
    let override_val = ["QONTINUI_WORKTREE_ROOT", "COORD_WORKTREE_ROOT"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok());
    agent_worktree_root_inner(canonical, override_val.as_deref())
}

/// Pure core of [`agent_worktree_root`]: env reading is lifted to the public
/// wrapper so this can be unit-tested deterministically without mutating the
/// process-global environment (which would be flaky under parallel tests).
/// `override_val` is the first present env value (or `None`); only an
/// *absolute* value is honored.
fn agent_worktree_root_inner(canonical: &Path, override_val: Option<&str>) -> PathBuf {
    if let Some(s) = override_val {
        let p = PathBuf::from(s.trim());
        if p.is_absolute() {
            return p;
        }
    }
    canonical
        .parent()
        .unwrap_or(canonical)
        .join(WORKTREE_ROOT_DIRNAME)
}

/// The directory name of the external agent-worktree root, sibling to the
/// workspace root. Single source of truth for [`agent_worktree_root`] and
/// [`allocated_worktree_for_path`].
pub const WORKTREE_ROOT_DIRNAME: &str = "qontinui-worktrees";

/// If `path` sits inside an ALREADY-ALLOCATED agent worktree that is still
/// live on disk, return that worktree's root directory.
///
/// # Why this exists
///
/// Session restore re-invokes the terminal-spawn path with the pane's recorded
/// working dir. When that dir IS a prior allocation, `acquire_for_terminal`
/// used to allocate all over again — coord mints a fresh `agent_id` on every
/// `POST /agents/allocate`, and the local target is
/// `agent_worktree_root/<agent_id>/<repo>`, so the id being fresh made a new
/// directory and a new branch **inevitable**. Every restart therefore leaked
/// one worktree and one branch per restored session, and the old ones were
/// never reused nor removed.
///
/// # What "still live" means
///
/// The layout is `<root>/<agent_id>/<repo_name>`, and a git worktree always
/// carries a `.git` **file** pointing back at the parent checkout's
/// `worktrees/<name>` directory. So the probe is: under the worktree root, at
/// the right depth, and `.git` present. If the operator deleted the directory,
/// or `git worktree remove`d it, `.git` is gone and this returns `None` — which
/// is the "genuinely gone" case where allocating a replacement is correct.
///
/// A bare-existing directory with no `.git` is deliberately NOT reused: git
/// would refuse `worktree add` onto it anyway, and handing a PTY a directory
/// that only LOOKS like a checkout is worse than allocating.
/// Case- and separator-insensitive path equality, for comparing a path the
/// probe REBUILT against the one a caller supplied.
///
/// `Path::==` is byte equality, which says `D: != D:/a/b` and
/// `.../Agent-B7 != .../agent-b7` — both of which reach the terminal-spawn path
/// routinely on Windows.
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    normalize_for_compare(&a.to_string_lossy()) == normalize_for_compare(&b.to_string_lossy())
}

pub fn allocated_worktree_for_path(path: &Path) -> Option<PathBuf> {
    let workspace_root = crate::workspace_paths::runner_workspace_root().into_root()?;
    // `agent_worktree_root` is defined per canonical checkout, but every
    // canonical checkout is a direct child of the workspace root, so the root
    // it resolves is the same for all of them. Passing a synthetic child
    // reuses that resolver -- env overrides included -- instead of
    // reimplementing it and drifting from it.
    let worktree_root = agent_worktree_root(&workspace_root.join(WORKTREE_ROOT_DIRNAME));
    allocated_worktree_for_path_in(&worktree_root, path)
}

/// Pure core of [`allocated_worktree_for_path`], with the worktree root and the
/// liveness probe injected so it is unit-testable without touching the
/// environment or the filesystem.
fn allocated_worktree_for_path_in(worktree_root: &Path, path: &Path) -> Option<PathBuf> {
    allocated_worktree_for_path_with(worktree_root, path, |p| p.join(".git").exists())
}

fn allocated_worktree_for_path_with(
    worktree_root: &Path,
    path: &Path,
    is_live: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    // Compared SEGMENT BY SEGMENT, not by byte offset into a lowercased
    // string. Two reasons the byte-offset form was wrong: `to_lowercase` is not
    // length-preserving in Unicode, so slicing `path` at `root_norm.len()`
    // could land off a char boundary and PANIC for a non-ASCII workspace root;
    // and a bare `starts_with` has no separator boundary, so
    // `.../qontinui-worktrees-old/a/b` matched the `.../qontinui-worktrees`
    // root and built a nonsense candidate.
    //
    // The comparison is case-insensitive (Windows hands us `D:/` or `d:/`) but
    // the segments are taken from the ORIGINAL path, so a case-SENSITIVE
    // filesystem is not handed a lowercased directory that does not exist.
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let mut root_parts = worktree_root
        .to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .into_iter();
    let path_str = path.to_string_lossy().replace('\\', "/");
    let mut path_parts = path_str.split('/').filter(|s| !s.is_empty());

    for rp in root_parts.by_ref() {
        let pp = path_parts.next()?;
        if norm(pp) != norm(&rp) {
            return None;
        }
    }
    // `path` must be the `<agent_id>/<repo_name>` dir or a descendant of it.
    let agent_id = path_parts.next()?;
    let repo_name = path_parts.next()?;

    let candidate = worktree_root.join(agent_id).join(repo_name);
    if is_live(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

/// Reverse of [`default_canonical_path`]: given an absolute filesystem
/// `path`, return the bare repo slug whose canonical checkout owns it, or
/// `None` if `path` is not under any known canonical checkout in the
/// runner's flat `<root>/<name>/` layout.
///
/// Algorithm (no hard-coded repo list — the layout is uniform):
/// 1. Take the first path segment under the workspace root (`<root>/<seg>`).
/// 2. Reconstruct that segment's canonical path via [`default_canonical_path`].
/// 3. Confirm `path` is the canonical path itself or a descendant of it
///    (an ancestor check that tolerates trailing components, worktree
///    subdirs, etc.).
///
/// Comparison is best-effort case-insensitive on the prefix to tolerate
/// Windows' case-insensitive drive paths (`D:/` vs `d:/`) without needing
/// the path to exist on disk (so this works for not-yet-materialized
/// paths and in unit tests). Both inputs are compared after lexical
/// normalization of separators.
///
/// Used by Layer 2 of the shared-checkout coordination plan
/// (`2026-06-03-shared-checkout-coordination-gap-fix.md`): when a terminal
/// session's `intent_repo` is `None`, derive it from the session's
/// `working_dir` so `acquire_for_terminal` can route through isolated
/// worktree acquisition when `QONTINUI_AGENT_WORKTREE_MODE` is on. Also
/// used by Layer 3 to resolve a Terminal-PTY session's repo for the coord
/// worktree-claim lookup.
pub fn repo_slug_for_path(path: &Path) -> Option<String> {
    let root = crate::workspace_paths::runner_workspace_root().into_root()?;
    repo_slug_for_path_in(&root, path)
}

/// Pure core of [`repo_slug_for_path`], with the workspace root injected.
///
/// This one **degrades** rather than failing closed even though
/// [`default_canonical_path`] does the opposite: answering "which repo owns this
/// path" neither creates nor executes anything, and its `Option` return already
/// means "not under a known checkout". A caller that then materializes something
/// goes through [`default_canonical_path`], which is where the fail-closed
/// disposition belongs.
fn repo_slug_for_path_in(root: &Path, path: &Path) -> Option<String> {
    // Lexically normalize `path` to forward slashes + lowercase for a
    // robust, on-disk-independent prefix comparison.
    let norm = normalize_for_compare(&path.to_string_lossy());

    // The first path segment under the workspace root is the candidate slug.
    // The root is now injected, but the walk still earns its keep: it is what
    // identifies WHICH segment is the slug, for a `path` at arbitrary depth
    // below the checkout. Try each leading segment as a candidate and accept the
    // one whose reconstructed canonical path is an ancestor of `path`.
    //
    // In practice the canonical path is `<root>/<seg>`, so the candidate
    // is the single segment immediately following the workspace root. We
    // recover it by walking ancestors: for each ancestor `a` of `path`,
    // `a.file_name()` is a candidate slug, and `default_canonical_path`
    // of that slug must equal `a` (normalized) for it to be the owning
    // checkout root.
    let mut ancestor = Some(path);
    while let Some(a) = ancestor {
        if let Some(name) = a.file_name().and_then(|n| n.to_str()) {
            if let Ok(canonical) = default_canonical_path_in(root, name) {
                let canon_norm = normalize_for_compare(&canonical.to_string_lossy());
                // `a` is the canonical checkout root for `name` iff its
                // normalized path equals the reconstructed canonical path,
                // AND the original `path` is that root or a descendant
                // (guaranteed because `a` is an ancestor of `path`).
                if canon_norm == normalize_for_compare(&a.to_string_lossy())
                    && (norm == canon_norm || norm.starts_with(&format!("{canon_norm}/")))
                {
                    return Some(name.to_string());
                }
            }
        }
        ancestor = a.parent();
    }
    None
}

/// Lowercase + forward-slash normalization for case/separator-insensitive
/// path-prefix comparison that does NOT require the path to exist on disk
/// (so it works for not-yet-created worktree paths and in unit tests).
/// Strips a single trailing slash so `<root>/<name>` and `<root>/<name>/`
/// compare equal.
fn normalize_for_compare(s: &str) -> String {
    let lowered = s.replace('\\', "/").to_lowercase();
    lowered.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_slug_passes_through() {
        assert_eq!(
            canonical_segment("qontinui-runner").unwrap(),
            "qontinui-runner"
        );
    }

    #[test]
    fn owner_name_reduces_to_name() {
        assert_eq!(
            canonical_segment("qontinui/qontinui-runner").unwrap(),
            "qontinui-runner"
        );
    }

    #[test]
    fn fork_owner_is_ignored() {
        // Coord's slug rule doesn't pin the owner — a fork's owner still
        // maps to the same flat checkout name.
        assert_eq!(
            canonical_segment("forkowner/qontinui-runner").unwrap(),
            "qontinui-runner"
        );
    }

    #[test]
    fn multi_level_takes_last_segment() {
        assert_eq!(canonical_segment("multi/level/path").unwrap(), "path");
    }

    #[test]
    fn ssh_url_falls_through_to_basename() {
        // Proves reuse of repo_basename_from_url: a naive rsplit_once('/')
        // would return "qontinui-runner.git" from the colon-delimited form.
        assert_eq!(
            canonical_segment("git@github.com:qontinui/qontinui-runner.git").unwrap(),
            "qontinui-runner"
        );
    }

    #[test]
    fn https_url_falls_through_to_basename() {
        assert_eq!(
            canonical_segment("https://github.com/qontinui/qontinui-runner.git").unwrap(),
            "qontinui-runner"
        );
    }

    #[test]
    fn empty_input_errors() {
        assert!(canonical_segment("").is_err());
    }

    #[test]
    fn whitespace_only_errors() {
        assert!(canonical_segment("   ").is_err());
    }

    #[test]
    fn trailing_slash_errors() {
        // Must not silently become `<root>/` or reinterpret the owner as
        // the name.
        assert!(canonical_segment("qontinui/").is_err());
    }

    /// A synthetic workspace root, never this machine's.
    ///
    /// Before slice 1 these assertions pinned `D:/qontinui-root` (and
    /// `$HOME`-or-`/tmp` on POSIX) because the resolution was hardcoded in the
    /// function under test. It no longer is, so a test that named a real
    /// location would only be asserting what this box happens to resolve to —
    /// which is exactly the machine dependence the plan removes. The layout rule
    /// (`<root>/<name>`) is what these tests own; *finding* the root is
    /// `qontinui_types::paths`' job and is tested there.
    fn test_root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("Z:/synthetic-workspace-root")
        } else {
            PathBuf::from("/synthetic-workspace-root")
        }
    }

    #[test]
    fn default_path_normalizes_both_slug_shapes() {
        let root = test_root();
        let bare = default_canonical_path_in(&root, "qontinui-runner").unwrap();
        let owner_name = default_canonical_path_in(&root, "qontinui/qontinui-runner").unwrap();
        assert_eq!(bare, owner_name);
    }

    #[test]
    fn default_path_is_the_repo_name_directly_under_the_root() {
        let root = test_root();
        assert_eq!(
            default_canonical_path_in(&root, "qontinui/qontinui-runner").unwrap(),
            root.join("qontinui-runner")
        );
    }

    #[test]
    fn default_path_propagates_validation_error() {
        let root = test_root();
        assert!(default_canonical_path_in(&root, "").is_err());
        assert!(default_canonical_path_in(&root, "qontinui/").is_err());
        // And through the env-reading wrapper, which validates the slug BEFORE
        // resolving the root so a caller bug is never reported as an
        // unresolvable workspace root.
        let err = default_canonical_path("qontinui/").unwrap_err();
        assert!(
            err.contains("empty name segment"),
            "a malformed slug must name the slug, not the workspace root: {err}"
        );
    }

    #[test]
    fn repo_slug_for_checkout_root() {
        let root = test_root();
        let checkout = default_canonical_path_in(&root, "qontinui-runner").unwrap();
        assert_eq!(
            repo_slug_for_path_in(&root, &checkout),
            Some("qontinui-runner".to_string())
        );
    }

    #[test]
    fn repo_slug_for_descendant_path() {
        let root = test_root();
        let mut p = default_canonical_path_in(&root, "qontinui-runner").unwrap();
        p.push("src-tauri");
        p.push("src");
        p.push("commands");
        p.push("terminal.rs");
        assert_eq!(
            repo_slug_for_path_in(&root, &p),
            Some("qontinui-runner".to_string())
        );
    }

    #[test]
    fn repo_slug_for_another_repo() {
        let root = test_root();
        let mut p = default_canonical_path_in(&root, "qontinui-coord").unwrap();
        p.push("src");
        assert_eq!(
            repo_slug_for_path_in(&root, &p),
            Some("qontinui-coord".to_string())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn repo_slug_is_case_insensitive_on_windows() {
        // Windows drive paths are case-insensitive; a lowercase-drive
        // working_dir must still resolve against the resolved root's casing.
        let root = test_root();
        let p = PathBuf::from("z:/synthetic-workspace-root/qontinui-runner/src-tauri");
        assert_eq!(
            repo_slug_for_path_in(&root, &p),
            Some("qontinui-runner".to_string())
        );
    }

    #[test]
    fn repo_slug_for_a_sibling_of_the_root_is_none() {
        // A path one level too high (the root itself) or beside the workspace
        // must not match — the slug is the segment DIRECTLY under the root.
        let root = test_root();
        assert_eq!(repo_slug_for_path_in(&root, &root), None);
    }

    #[test]
    fn agent_worktree_root_end_user_single_project() {
        // End-user single-project layout: the project root has no sibling
        // repos — the worktree root is a sibling `qontinui-worktrees` dir,
        // OUTSIDE the project checkout (never under it).
        let canonical = Path::new("C:/Users/alice/myapp");
        let got = agent_worktree_root_inner(canonical, None);
        assert_eq!(got, PathBuf::from("C:/Users/alice/qontinui-worktrees"));
        assert!(!got.starts_with(canonical), "must be outside the project");
    }

    #[test]
    fn agent_worktree_root_operator_multi_repo() {
        // Operator multi-repo layout: many sibling checkouts under
        // D:/qontinui-root — all agents' worktrees share one sibling root.
        let canonical = Path::new("D:/qontinui-root/qontinui-coord");
        let got = agent_worktree_root_inner(canonical, None);
        assert_eq!(got, PathBuf::from("D:/qontinui-root/qontinui-worktrees"));
        assert!(!got.starts_with(canonical), "must be outside the repo");
    }

    #[test]
    fn agent_worktree_root_honors_absolute_override() {
        // An absolute env override takes full control of the root.
        let canonical = Path::new("D:/qontinui-root/qontinui-coord");
        let abs = if cfg!(windows) {
            "E:/custom-wt-root"
        } else {
            "/srv/custom-wt-root"
        };
        assert_eq!(
            agent_worktree_root_inner(canonical, Some(abs)),
            PathBuf::from(abs)
        );
    }

    #[test]
    fn agent_worktree_root_ignores_relative_override() {
        // A RELATIVE override is ignored (it would reintroduce the
        // ambient-cwd bug) — falls back to the sibling default.
        let canonical = Path::new("D:/qontinui-root/qontinui-coord");
        assert_eq!(
            agent_worktree_root_inner(canonical, Some("relative/wt")),
            PathBuf::from("D:/qontinui-root/qontinui-worktrees")
        );
    }

    #[test]
    fn agent_worktree_root_full_path_is_outside_canonical() {
        // The composed worktree path
        // `<root>/<agent_id>/<repo>` must not sit under canonical.
        let canonical = Path::new("D:/qontinui-root/qontinui-coord");
        let full = agent_worktree_root_inner(canonical, None)
            .join("019e-agent")
            .join("qontinui-types");
        assert!(!full.starts_with(canonical));
        assert_eq!(
            full,
            PathBuf::from("D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-types")
        );
    }

    #[test]
    fn repo_slug_outside_workspace_is_none() {
        // A path that doesn't sit under any `<root>/<name>/` checkout must
        // not falsely match.
        let p = PathBuf::from("/some/unrelated/place/foo/bar");
        assert_eq!(repo_slug_for_path_in(&test_root(), &p), None);
    }
}

// ===========================================================================
// Worktree REUSE probe (manual-test-loop iter 16)
//
// The defect these pin: session restore replayed a pane's recorded working
// dir into `acquire_for_terminal`, which allocated unconditionally. Coord
// mints a fresh `agent_id` per allocate and the target is
// `<root>/<agent_id>/<repo>`, so every restart made a NEW directory and a NEW
// branch — one leaked worktree per restored session per restart.
//
// The liveness probe is injected in these tests, so they assert the PATH
// arithmetic and the live/gone decision without touching the filesystem.
// ===========================================================================
#[cfg(test)]
mod allocated_worktree_probe_tests {
    use super::*;

    const ROOT: &str = "D:/qontinui-root/qontinui-worktrees";

    fn live(_p: &Path) -> bool {
        true
    }
    fn gone(_p: &Path) -> bool {
        false
    }

    /// POSITIVE: the exact `<root>/<agent_id>/<repo>` dir is recognised and
    /// returned, so restore reuses it instead of allocating.
    #[test]
    fn a_live_allocation_is_recognised_and_returned() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new("D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner"),
            live,
        );
        assert_eq!(
            got,
            Some(PathBuf::from(
                "D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner"
            ))
        );
    }

    /// A pane whose cwd is a SUBDIRECTORY of the allocation still resolves to
    /// the allocation root — a terminal is routinely left somewhere deeper.
    #[test]
    fn a_descendant_resolves_to_the_allocation_root() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new(
                "D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner/src-tauri/src",
            ),
            live,
        );
        assert_eq!(
            got,
            Some(PathBuf::from(
                "D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner"
            ))
        );
    }

    /// NEGATIVE and load-bearing: a recorded worktree that is GONE must NOT be
    /// reused. This is the arm that keeps a fresh allocation possible, and
    /// without it the fix would strand restored sessions in a dead directory.
    #[test]
    fn a_deleted_allocation_is_not_reused() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new("D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner"),
            gone,
        );
        assert_eq!(
            got, None,
            "a worktree whose .git is gone must be re-allocated"
        );
    }

    /// NEGATIVE: an ordinary canonical checkout is not an allocation, so a
    /// normal (non-restored) session still routes through allocate.
    #[test]
    fn a_canonical_checkout_is_not_an_allocation() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new("D:/qontinui-root/qontinui-runner"),
            live,
        );
        assert_eq!(got, None);
    }

    /// NEGATIVE: the worktree root itself, and a bare `<agent_id>` with no
    /// repo segment, are both incomplete — reusing either would hand the PTY
    /// a directory that is not a checkout.
    #[test]
    fn an_incomplete_path_under_the_root_is_rejected() {
        assert_eq!(
            allocated_worktree_for_path_with(Path::new(ROOT), Path::new(ROOT), live),
            None,
            "the root itself is not an allocation"
        );
        assert_eq!(
            allocated_worktree_for_path_with(
                Path::new(ROOT),
                Path::new("D:/qontinui-root/qontinui-worktrees/019e-agent"),
                live,
            ),
            None,
            "an agent id with no repo segment is not an allocation"
        );
    }

    /// The rebuilt path must keep the SEGMENTS' original case.
    ///
    /// The probe compares case-insensitively (Windows hands us `D:/` or `d:/`),
    /// but it used to rebuild from the lowercased string too. On a
    /// case-sensitive filesystem that yields a directory that does not exist,
    /// the liveness probe says "gone", and the restore silently allocates a
    /// fresh worktree again -- the exact leak this fix closes, reintroduced on
    /// Linux only.
    #[test]
    fn the_rebuilt_path_preserves_segment_case() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new("D:/qontinui-root/qontinui-worktrees/Agent-B7/Qontinui-Runner"),
            live,
        );
        assert_eq!(
            got,
            Some(PathBuf::from(
                "D:/qontinui-root/qontinui-worktrees/Agent-B7/Qontinui-Runner"
            )),
            "segments must not be lowercased on the way back out"
        );
    }

    /// A sibling directory whose name merely STARTS WITH the worktree root's
    /// name is not under the root. The byte-prefix form matched it and built a
    /// nonsense candidate out of the wrong segments.
    #[test]
    fn a_sibling_root_with_a_shared_prefix_is_not_matched() {
        assert_eq!(
            allocated_worktree_for_path_with(
                Path::new(ROOT),
                Path::new("D:/qontinui-root/qontinui-worktrees-old/019e-agent/qontinui-runner"),
                live,
            ),
            None,
            "`qontinui-worktrees-old` is a different directory from `qontinui-worktrees`"
        );
    }

    /// The segment walk must not panic on a non-ASCII root. The byte-offset
    /// form sliced `path` at `root_norm.len()`, and `to_lowercase` is not
    /// length-preserving in Unicode (U+0130 grows), so that could land off a
    /// char boundary.
    #[test]
    fn a_non_ascii_root_does_not_panic() {
        let root = "D:/İstanbul-root/qontinui-worktrees";
        let got = allocated_worktree_for_path_with(
            Path::new(root),
            Path::new("D:/İstanbul-root/qontinui-worktrees/019e-agent/qontinui-runner"),
            live,
        );
        assert_eq!(
            got,
            Some(PathBuf::from(
                "D:/İstanbul-root/qontinui-worktrees/019e-agent/qontinui-runner"
            ))
        );
    }

    /// `paths_equal` is what the reuse gate uses to insist the recorded dir IS
    /// the allocation root rather than a descendant of it, and it has to see
    /// through Windows' separator and case variance to do that.
    #[test]
    fn paths_equal_sees_through_separator_and_case() {
        assert!(paths_equal(
            Path::new("D:/qontinui-root/qontinui-worktrees/A/qontinui-runner"),
            Path::new(r"d:\QONTINUI-ROOT\qontinui-worktrees\a\qontinui-runner"),
        ));
        assert!(!paths_equal(
            Path::new("D:/qontinui-root/qontinui-worktrees/A/qontinui-runner"),
            Path::new("D:/qontinui-root/qontinui-worktrees/A/qontinui-runner/src-tauri"),
        ));
    }

    /// Windows paths reach this code with either separator and either case.
    #[test]
    fn separators_and_case_do_not_defeat_the_probe() {
        let got = allocated_worktree_for_path_with(
            Path::new(ROOT),
            Path::new(r"d:\QONTINUI-ROOT\qontinui-worktrees\019e-agent\qontinui-runner"),
            live,
        );
        assert_eq!(
            got,
            Some(PathBuf::from(
                "D:/qontinui-root/qontinui-worktrees/019e-agent/qontinui-runner"
            )),
            "the returned path is rebuilt from the caller's root, so it keeps the \
             root's canonical casing"
        );
    }
}
