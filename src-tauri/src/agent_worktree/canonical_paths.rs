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

use std::path::PathBuf;

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
}
