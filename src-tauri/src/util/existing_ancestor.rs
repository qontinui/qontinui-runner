//! Resolve a path through its nearest EXISTING ancestor.
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
//! One walk ([`walk`]) serves three callers, which differ in two knobs:
//!
//! - WHERE the walk may stop: at the nearest existing DIRECTORY (the snapshot
//!   reader and the census), or at the nearest existing PATH of any kind (the
//!   `assert_inside` test guard, which must canonicalise an existing leaf FILE
//!   so a symlinked file pointing outside its fixture is caught).
//! - What a component that EXISTS AS A SYMLINK but fails the stop predicate
//!   means (a dangling link, a link loop, or — in directory mode — a link to a
//!   file). The snapshot reader climbs past it, as it always has. The
//!   resolving callers REFUSE it: climbing past would rejoin the link's name
//!   lexically below its parent and report that as "resolved", although the
//!   link was never followed.
//!
//! Plan `2026-11-22-coord-charges-a-build-in-a-symlinked-agent-worktree-to-root`
//! lifted this out of `mcp::snapshots` so the worktree census could use the
//! same walk instead of growing a second copy.

use std::path::{Path, PathBuf};

/// Where the walk stops, and how strict it is about unfollowable links.
#[derive(Clone, Copy)]
struct Walk {
    /// Stop at any existing path (file or directory), not only a directory.
    stop_at_any_path: bool,
    /// Refuse a component that exists as a symlink but fails the stop
    /// predicate, instead of climbing past it.
    refuse_unfollowable_link: bool,
}

/// The one ancestor walk: from `start` up to the nearest component that
/// satisfies `walk`'s stop predicate, canonicalised, with the remainder of
/// `path` below it.
fn walk(path: &Path, start: Option<&Path>, walk: Walk) -> Result<(PathBuf, PathBuf), String> {
    let mut dir = start;
    while let Some(d) = dir {
        let stops = if walk.stop_at_any_path {
            d.exists()
        } else {
            d.is_dir()
        };
        if stops {
            break;
        }
        if walk.refuse_unfollowable_link
            && d.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(format!(
                "{} is a symlink that cannot be followed (dangling, looping, or not a directory); \
                 {} is not resolvable",
                d.display(),
                path.display()
            ));
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

/// Walk from `start` up to the nearest directory that exists, canonicalise
/// it, and return `(canonical directory, path relative to that directory)`.
///
/// `start` must be `path` itself or one of its ancestors: the snapshot reader
/// starts at the PARENT (a symlinked leaf is recorded under its own name, so
/// it must not be followed). `None` — a path with no parent — is reported as
/// having no existing ancestor. A component that is an unfollowable symlink is
/// climbed past (the snapshot reader's historical behaviour).
///
/// Errors are human-readable and name the path; they occur when no ancestor
/// exists, when `start` is not a prefix of `path`, or when canonicalising the
/// ancestor fails (a permission error, a dangling mount).
pub fn split_at_canonical_dir(
    path: &Path,
    start: Option<&Path>,
) -> Result<(PathBuf, PathBuf), String> {
    walk(
        path,
        start,
        Walk {
            stop_at_any_path: false,
            refuse_unfollowable_link: false,
        },
    )
}

fn joined((canon, rest): (PathBuf, PathBuf)) -> PathBuf {
    // `join("")` would append a trailing separator; an empty remainder means
    // the path itself was the existing ancestor.
    if rest.as_os_str().is_empty() {
        canon
    } else {
        canon.join(rest)
    }
}

/// `path` with every symlink and junction in its existing prefix resolved:
/// the canonicalised nearest existing directory among `path` and its
/// ancestors, joined with the lexical remainder. A `path` that is itself an
/// existing directory (or a link to one) resolves completely.
///
/// A component that exists as a symlink but cannot be followed to a directory
/// (a dangling link, a loop) is an ERROR, not a reason to climb: the result
/// would otherwise name the link's parent joined with the link's own name and
/// claim to be resolved. The census reads an error as NOT OBSERVED.
///
/// The result carries whatever form `canonicalize` produces — on Windows that
/// includes the `\\?\` verbatim prefix, which a caller comparing against plain
/// drive-letter paths must strip.
pub fn resolve_through_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    walk(
        path,
        Some(path),
        Walk {
            stop_at_any_path: false,
            refuse_unfollowable_link: true,
        },
    )
    .map(joined)
}

/// Like [`resolve_through_existing_ancestor`], but the walk stops at the
/// nearest existing PATH of any kind, so an existing leaf FILE is itself
/// canonicalised — a symlinked file resolves to its target. Used by guards
/// that must catch a symlinked file pointing outside a root.
pub fn resolve_through_existing_path(path: &Path) -> Result<PathBuf, String> {
    walk(
        path,
        Some(path),
        Walk {
            stop_at_any_path: true,
            refuse_unfollowable_link: true,
        },
    )
    .map(joined)
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
        assert!(resolve_through_existing_path(p).is_err());
    }

    #[test]
    fn an_existing_file_stops_the_path_walk_but_not_the_dir_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let canon_root = std::fs::canonicalize(tmp.path()).unwrap();
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            resolve_through_existing_path(&file).unwrap(),
            canon_root.join("f.txt")
        );
        // The directory walk climbs past the (non-link) file to its parent.
        assert_eq!(
            resolve_through_existing_ancestor(&file).unwrap(),
            canon_root.join("f.txt")
        );
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

    /// A5: an existing leaf FILE that is a symlink resolves to its target
    /// under the path walk, so a guard comparing against a root catches a
    /// file pointing outside it.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_resolves_to_its_target_under_the_path_walk() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret");
        std::fs::write(&target, "x").unwrap();
        let link = inside.path().join("roster.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let resolved = resolve_through_existing_path(&link).unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&target).unwrap());
        assert!(!resolved.starts_with(std::fs::canonicalize(inside.path()).unwrap()));
    }

    /// A10: a dangling symlink in the prefix is NOT climbed past by the
    /// resolving walks — the result would be the link's parent joined with the
    /// link's name, reported as resolved although the link was never followed.
    /// The snapshot split keeps its historical climb.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_in_the_prefix_is_unresolvable() {
        let tmp = tempfile::tempdir().unwrap();
        let canon_root = std::fs::canonicalize(tmp.path()).unwrap();
        let link = tmp.path().join("agent-worktrees-01a");
        std::os::unix::fs::symlink(tmp.path().join("gone"), &link).unwrap();
        let below = link.join("qontinui-runner").join("target");

        for p in [&link, &below] {
            let err = resolve_through_existing_ancestor(p).unwrap_err();
            assert!(err.contains("cannot be followed"), "{err}");
            let err = resolve_through_existing_path(p).unwrap_err();
            assert!(err.contains("cannot be followed"), "{err}");
        }

        // Snapshot semantics unchanged: the split climbs past the link.
        let (dir, rest) = split_at_canonical_dir(&below, below.parent()).unwrap();
        assert_eq!(dir, canon_root);
        assert_eq!(
            rest,
            PathBuf::from("agent-worktrees-01a")
                .join("qontinui-runner")
                .join("target")
        );
    }
}
