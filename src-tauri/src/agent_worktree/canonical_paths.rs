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

use crate::observable_bridge::git_ops::repo_basename_from_url;

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
/// (`crate::observable_bridge::git_ops`) — it already
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
        .join("qontinui-worktrees")
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
