//! Resolve a path through its nearest EXISTING directory ancestor.
//!
//! `Path::canonicalize` fails on a path that does not exist yet — a cargo
//! target dir that has never been built into, a file a session has since
//! deleted — yet such a path still lives on a definite volume: the one its
//! nearest existing ancestor resolves to. These helpers canonicalise that
//! ancestor (resolving every symlink and junction in the existing prefix) and
//! rejoin the lexical remainder below it.
//!
//! The remainder is joined LEXICALLY, so a caller that wants `..` in it to
//! mean anything must normalise the path before calling; every caller here
//! passes an already-normalised absolute path.
//!
//! Plan `2026-11-22-coord-charges-a-build-in-a-symlinked-agent-worktree-to-root`
//! lifted this out of `mcp::snapshots` so the worktree census could use the
//! same walk instead of growing a second copy.

use std::path::{Path, PathBuf};

/// Walk from `start` up to the nearest directory that exists, canonicalise
/// it, and return `(canonical directory, path relative to that directory)`.
///
/// `start` must be `path` itself or one of its ancestors: the snapshot reader
/// starts at the PARENT (a symlinked leaf is recorded under its own name, so
/// it must not be followed), the census at the path itself (a symlinked
/// worktree dir is exactly what must be followed). `None` — a path with no
/// parent — is reported as having no existing ancestor.
///
/// Errors are human-readable and name the path; they occur when no ancestor
/// exists, when `start` is not a prefix of `path`, or when canonicalising the
/// ancestor fails (a permission error, a dangling mount).
pub fn split_at_canonical_dir(
    path: &Path,
    start: Option<&Path>,
) -> Result<(PathBuf, PathBuf), String> {
    let mut dir = start;
    while let Some(d) = dir {
        if d.is_dir() {
            break;
        }
        dir = d.parent();
    }
    let dir = dir.ok_or_else(|| format!("no existing ancestor of {}", path.display()))?;
    let rest = path
        .strip_prefix(dir)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_path_buf();
    let canon_dir =
        std::fs::canonicalize(dir).map_err(|e| format!("canonicalize {}: {e}", dir.display()))?;
    Ok((canon_dir, rest))
}

/// `path` with every symlink and junction in its existing prefix resolved:
/// the canonicalised nearest existing directory among `path` and its
/// ancestors, joined with the lexical remainder. A `path` that is itself an
/// existing directory (or a link to one) resolves completely.
///
/// The result carries whatever form `canonicalize` produces — on Windows that
/// includes the `\\?\` verbatim prefix, which a caller comparing against plain
/// drive-letter paths must strip.
pub fn resolve_through_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let (canon_dir, rest) = split_at_canonical_dir(path, Some(path))?;
    // `join("")` would append a trailing separator; an empty remainder means
    // the path itself was the existing directory.
    Ok(if rest.as_os_str().is_empty() {
        canon_dir
    } else {
        canon_dir.join(rest)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_dir_resolves_to_its_canonical_form() {
        let tmp = tempfile::tempdir().unwrap();
        let canon_root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(
            resolve_through_existing_ancestor(&dir).unwrap(),
            canon_root.join("a")
        );
    }

    #[test]
    fn a_missing_tail_is_rejoined_below_the_nearest_existing_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let canon_root = std::fs::canonicalize(tmp.path()).unwrap();
        let missing = tmp.path().join("x").join("y").join("z");
        let (dir, rest) = split_at_canonical_dir(&missing, Some(&missing)).unwrap();
        assert_eq!(dir, canon_root);
        assert_eq!(rest, PathBuf::from("x").join("y").join("z"));
        assert_eq!(
            resolve_through_existing_ancestor(&missing).unwrap(),
            canon_root.join("x").join("y").join("z")
        );
    }

    #[test]
    fn the_walk_starts_at_start_not_at_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let canon_root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        // `dir` exists, but starting from its parent stops at the parent.
        let (got, rest) = split_at_canonical_dir(&dir, dir.parent()).unwrap();
        assert_eq!(got, canon_root);
        assert_eq!(rest, PathBuf::from("d"));
    }

    #[test]
    fn no_start_is_no_existing_ancestor() {
        let err = split_at_canonical_dir(Path::new("/x"), None).unwrap_err();
        assert!(err.starts_with("no existing ancestor of"), "{err}");
    }

    #[test]
    fn a_relative_path_with_no_existing_ancestor_is_an_error() {
        let p = Path::new("definitely-not-here-7f3a/child");
        assert!(resolve_through_existing_ancestor(p).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_dir_resolves_to_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("data").join("wt");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canon_real = std::fs::canonicalize(&real).unwrap();

        assert_eq!(
            resolve_through_existing_ancestor(&link).unwrap(),
            canon_real
        );
        // A not-yet-existing child of the link resolves through it too.
        assert_eq!(
            resolve_through_existing_ancestor(&link.join("repo").join("target")).unwrap(),
            canon_real.join("repo").join("target")
        );
    }
}
