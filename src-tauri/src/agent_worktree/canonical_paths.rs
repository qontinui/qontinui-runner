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

/// Build the default canonical-checkout path for `repo` on this host.
/// Windows: `D:/qontinui-root/<name>/`. The slug is normalized via
/// [`canonical_segment`], so both `qontinui-runner` and
/// `qontinui/qontinui-runner` resolve to the same on-disk location.
#[cfg(target_os = "windows")]
pub fn default_canonical_path(repo: &str) -> Result<PathBuf, String> {
    let segment = canonical_segment(repo)?;
    Ok(PathBuf::from(format!("D:/qontinui-root/{segment}")))
}

/// Build the default canonical-checkout path for `repo` on this host.
/// POSIX: `$HOME/qontinui-root/<name>/`. The slug is normalized via
/// [`canonical_segment`], so both `qontinui-runner` and
/// `qontinui/qontinui-runner` resolve to the same on-disk location.
#[cfg(not(target_os = "windows"))]
pub fn default_canonical_path(repo: &str) -> Result<PathBuf, String> {
    let segment = canonical_segment(repo)?;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    Ok(PathBuf::from(format!("{home}/qontinui-root/{segment}")))
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
    // Lexically normalize `path` to forward slashes + lowercase for a
    // robust, on-disk-independent prefix comparison.
    let norm = normalize_for_compare(&path.to_string_lossy());

    // The first path segment under the workspace root is the candidate
    // slug. We don't know the workspace root abstractly here, so we lean
    // on `default_canonical_path` round-tripping: try each leading segment
    // of `path` as a candidate slug and accept the one whose canonical
    // path is an ancestor of `path`.
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
            if let Ok(canonical) = default_canonical_path(name) {
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

    #[test]
    fn default_path_normalizes_both_slug_shapes() {
        let bare = default_canonical_path("qontinui-runner").unwrap();
        let owner_name = default_canonical_path("qontinui/qontinui-runner").unwrap();
        assert_eq!(bare, owner_name);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn default_path_windows_layout() {
        assert_eq!(
            default_canonical_path("qontinui/qontinui-runner").unwrap(),
            PathBuf::from("D:/qontinui-root/qontinui-runner")
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn default_path_posix_layout() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        assert_eq!(
            default_canonical_path("qontinui/qontinui-runner").unwrap(),
            PathBuf::from(format!("{home}/qontinui-root/qontinui-runner"))
        );
    }

    #[test]
    fn default_path_propagates_validation_error() {
        assert!(default_canonical_path("").is_err());
        assert!(default_canonical_path("qontinui/").is_err());
    }

    #[test]
    fn repo_slug_for_checkout_root() {
        let root = default_canonical_path("qontinui-runner").unwrap();
        assert_eq!(
            repo_slug_for_path(&root),
            Some("qontinui-runner".to_string())
        );
    }

    #[test]
    fn repo_slug_for_descendant_path() {
        let mut p = default_canonical_path("qontinui-runner").unwrap();
        p.push("src-tauri");
        p.push("src");
        p.push("commands");
        p.push("terminal.rs");
        assert_eq!(repo_slug_for_path(&p), Some("qontinui-runner".to_string()));
    }

    #[test]
    fn repo_slug_for_another_repo() {
        let mut p = default_canonical_path("qontinui-coord").unwrap();
        p.push("src");
        assert_eq!(repo_slug_for_path(&p), Some("qontinui-coord".to_string()));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn repo_slug_is_case_insensitive_on_windows() {
        // Windows drive paths are case-insensitive; a `d:/` working_dir
        // must still resolve against the `D:/` canonical path.
        let p = PathBuf::from("d:/qontinui-root/qontinui-runner/src-tauri");
        assert_eq!(repo_slug_for_path(&p), Some("qontinui-runner".to_string()));
    }

    #[test]
    fn repo_slug_outside_workspace_is_none() {
        // A path that doesn't sit under any `<root>/<name>/` checkout must
        // not falsely match. Use a path whose segments don't reconstruct
        // to themselves via `default_canonical_path` (a temp dir).
        let p = PathBuf::from("/some/unrelated/place/foo/bar");
        assert_eq!(repo_slug_for_path(&p), None);
    }
}
