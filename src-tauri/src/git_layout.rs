//! Where a checkout's git directories are, read from disk the way git reads
//! them — no `git` process.
//!
//! A primary checkout carries `.git` as a DIRECTORY. A linked worktree carries
//! `.git` as a FILE (`gitdir: <dir>`), and that per-worktree git dir names the
//! repository's shared COMMON dir in its `commondir` file. Remote-tracking refs
//! (`refs/remotes/*`), `packed-refs` and the remote config live in the common
//! dir, so every linked worktree of a repo shares them: a fetch from one is a
//! fetch for all, and a trunk ref resolved for one is the trunk ref for all.
//!
//! Plan `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! (Phase 1) lifted this out of the two lib-side copies that had grown it
//! independently (`env_agent::collectors` and `observable_bridge::git_ops`) so
//! the census and the tree publisher could key their per-repo work on it.

use std::path::{Path, PathBuf};

/// Read at most `max` bytes of a small file. `None` on any IO error, when the
/// path is not a regular file, and when the file is larger than `max` — a
/// pointer file that has grown past a few KB is not a pointer file, and an
/// unbounded read of a path an attacker could replace with a FIFO would hang
/// a walk (custody's original reason for the bound).
pub fn read_bounded(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The value of a `.git` FILE's `gitdir:` line — the FIRST line, as git reads
/// it — trimmed; `None` for any other content or a blank value.
pub fn gitdir_value(raw: &str) -> Option<&str> {
    let line = raw.lines().next()?.trim_end_matches('\r').trim();
    let rest = line.strip_prefix("gitdir:")?.trim();
    (!rest.is_empty()).then_some(rest)
}

/// Parse a `.git` FILE's contents into the git dir it names, resolving a
/// relative value against `base` (the checkout holding the `.git` file).
///
/// `is_absolute` is false for `C:/x` on POSIX builds and for `/x` on Windows,
/// so both shapes are spelled rather than trusting the platform rule.
pub fn parse_gitdir_line(raw: &str, base: &Path) -> Option<PathBuf> {
    let rest = gitdir_value(raw)?;
    let p = PathBuf::from(rest);
    let looks_absolute = p.is_absolute()
        || rest.starts_with('/')
        || rest.starts_with('\\')
        || rest
            .as_bytes()
            .get(1)
            .is_some_and(|c| *c == b':' && rest.as_bytes()[0].is_ascii_alphabetic());
    Some(if looks_absolute { p } else { base.join(p) })
}

/// Upper bound on a `.git` pointer file and a `commondir` file.
const POINTER_MAX_BYTES: u64 = 4096;

/// `<checkout>`'s `$GIT_DIR`, read from disk with no subprocess.
///
/// * `.git` is a **directory** ⇒ a primary checkout; the directory IS the git
///   dir.
/// * `.git` is a **file** ⇒ a linked worktree; its `gitdir:` line names the
///   git dir ([`parse_gitdir_line`]).
///
/// `None` when `.git` is absent or the file does not carry a `gitdir:` line.
/// A `gitdir:` naming a directory that does not exist is still returned —
/// whether it exists is the caller's question ([`git_provably_absent`]).
pub fn git_dir_for(checkout: &Path) -> Option<PathBuf> {
    let dot_git = checkout.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    let raw = read_bounded(&dot_git, POINTER_MAX_BYTES)?;
    parse_gitdir_line(&raw, checkout)
}

/// `<checkout>`'s git dir and git COMMON dir, read from disk.
///
/// The common dir is what the git dir's `commondir` file names (relative to
/// the git dir), or the git dir itself when there is none. `None` exactly when
/// [`git_dir_for`] is `None`.
pub fn git_dir_and_common_dir(checkout: &Path) -> Option<(PathBuf, PathBuf)> {
    let git_dir = git_dir_for(checkout)?;
    let common_dir = match read_bounded(&git_dir.join("commondir"), POINTER_MAX_BYTES) {
        Some(text) => {
            let t = text.trim();
            if t.is_empty() {
                git_dir.clone()
            } else if Path::new(t).is_absolute() {
                PathBuf::from(t)
            } else {
                git_dir.join(t)
            }
        }
        None => git_dir.clone(),
    };
    Some((git_dir, common_dir))
}

/// The git COMMON dir of `<checkout>`, canonicalised so two checkouts of one
/// repository compare equal. `None` when [`git_dir_and_common_dir`] is `None`
/// AND when the common dir it names does not exist (or cannot be resolved) — a
/// key for a repository git cannot open would group nothing real.
pub fn common_dir_key(checkout: &Path) -> Option<PathBuf> {
    let (_, common) = git_dir_and_common_dir(checkout)?;
    std::fs::canonicalize(&common).ok()
}

/// True only when EVERY `git` command run in `dir` must fail because git can
/// find no repository there — the two shapes where a git probe's answer is
/// known before it is asked:
///
/// 1. `dir/.git` is a gitdir FILE whose target directory does not exist (a
///    worktree whose admin dir was pruned). git refuses with "not a git
///    repository" and never falls back to a parent.
/// 2. Neither `dir` nor ANY ancestor of it has a `.git` entry or looks like a
///    bare repository (`HEAD` + `objects/` + `refs/`), so git's discovery walk
///    finds nothing.
///
/// Every reading it cannot take answers `false` (a probe may still run): an
/// unreadable `.git`, a `.git` that is neither file nor directory, a gitdir
/// file it cannot parse. That keeps the shortcut on the side of running the
/// probe — the shortcut only ever skips work whose result is already known.
pub fn git_provably_absent(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(meta) if meta.is_file() => {
            // Shape 1 — a gitdir file. Only a PARSEABLE one whose target is
            // measured absent is provably dead.
            match git_dir_and_common_dir(dir) {
                Some((git_dir, _)) => matches!(
                    std::fs::symlink_metadata(&git_dir),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound
                ),
                None => false,
            }
        }
        // A `.git` directory, symlink or anything else: git may well open it.
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Shape 2 — no `.git` here; git walks up. Provably absent only if
            // nothing up the chain could be a repository either.
            if may_be_bare_repo(dir) {
                return false;
            }
            let mut cur = dir.parent();
            while let Some(p) = cur {
                match std::fs::symlink_metadata(p.join(".git")) {
                    Ok(_) => return false,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return false,
                }
                if may_be_bare_repo(p) {
                    return false;
                }
                cur = p.parent();
            }
            true
        }
        Err(_) => false,
    }
}

/// Whether `dir` has the three entries git's discovery accepts as a bare
/// repository. Errors other than NotFound count as "may be" — fail toward
/// running the probe.
fn may_be_bare_repo(dir: &Path) -> bool {
    let present = |name: &str| match std::fs::symlink_metadata(dir.join(name)) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    };
    present("HEAD") && present("objects") && present("refs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dot_git_directory_is_both_dirs() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap();
        let (g, c) = git_dir_and_common_dir(t.path()).unwrap();
        assert_eq!(g, t.path().join(".git"));
        assert_eq!(c, t.path().join(".git"));
    }

    #[test]
    fn a_linked_worktree_reads_commondir_relative_and_absolute() {
        let t = tempfile::tempdir().unwrap();
        let common = t.path().join("primary").join(".git");
        let admin = common.join("worktrees").join("wt1");
        std::fs::create_dir_all(&admin).unwrap();
        let wt = t.path().join("wt1");
        std::fs::create_dir_all(&wt).unwrap();

        // Absolute gitdir, relative commondir (git's own default shape).
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let (g, c) = git_dir_and_common_dir(&wt).unwrap();
        assert_eq!(g, admin);
        assert_eq!(c, admin.join("../.."));
        assert_eq!(
            std::fs::canonicalize(c).unwrap(),
            std::fs::canonicalize(&common).unwrap()
        );

        // Absolute commondir.
        std::fs::write(admin.join("commondir"), format!("{}\n", common.display())).unwrap();
        let (_, c) = git_dir_and_common_dir(&wt).unwrap();
        assert_eq!(c, common);

        // No commondir → the git dir is its own common dir.
        std::fs::remove_file(admin.join("commondir")).unwrap();
        let (g, c) = git_dir_and_common_dir(&wt).unwrap();
        assert_eq!(g, c);
    }

    #[test]
    fn a_relative_gitdir_resolves_against_the_checkout() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("adm")).unwrap();
        let wt = t.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../adm\n").unwrap();
        let (g, _) = git_dir_and_common_dir(&wt).unwrap();
        assert_eq!(g, wt.join("../adm"));
    }

    #[test]
    fn unparseable_or_missing_dot_git_is_none() {
        let t = tempfile::tempdir().unwrap();
        assert!(git_dir_and_common_dir(t.path()).is_none(), "no .git at all");
        std::fs::write(t.path().join(".git"), "").unwrap();
        assert!(
            git_dir_and_common_dir(t.path()).is_none(),
            "empty .git file"
        );
        std::fs::write(t.path().join(".git"), "gitdir:   \n").unwrap();
        assert!(git_dir_and_common_dir(t.path()).is_none(), "blank gitdir");
    }

    #[test]
    fn two_worktrees_of_one_repo_share_a_common_dir_key() {
        let t = tempfile::tempdir().unwrap();
        let common = t.path().join("p").join(".git");
        for wt in ["a", "b"] {
            let admin = common.join("worktrees").join(wt);
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(admin.join("commondir"), "../..\n").unwrap();
            let d = t.path().join(wt);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        }
        let ka = common_dir_key(&t.path().join("a")).unwrap();
        let kb = common_dir_key(&t.path().join("b")).unwrap();
        let kp = common_dir_key(&t.path().join("p")).unwrap();
        assert_eq!(ka, kb);
        assert_eq!(ka, kp);
    }

    #[test]
    fn a_dangling_gitdir_file_is_provably_absent() {
        let t = tempfile::tempdir().unwrap();
        let wt = t.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let gone = t.path().join("gone-admin");
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gone.display())).unwrap();
        assert!(git_provably_absent(&wt));
        // Once the admin dir exists, git may open it: not provably absent.
        std::fs::create_dir_all(&gone).unwrap();
        assert!(!git_provably_absent(&wt));
    }

    #[test]
    fn a_husk_with_no_repo_anywhere_above_is_provably_absent() {
        let t = tempfile::tempdir().unwrap();
        let husk = t.path().join("husk");
        std::fs::create_dir_all(&husk).unwrap();
        // The answer depends on the temp dir's own ancestry, which the test
        // does not control — so state the premise and assert the answer it
        // implies, rather than skipping.
        let repo_above = husk.ancestors().skip(1).any(|p| {
            p.join(".git").exists()
                || (p.join("HEAD").exists()
                    && p.join("objects").exists()
                    && p.join("refs").exists())
        });
        assert_eq!(git_provably_absent(&husk), !repo_above);
        if !repo_above {
            // And creating a `.git` directory flips it.
            std::fs::create_dir(husk.join(".git")).unwrap();
            assert!(!git_provably_absent(&husk));
        }
    }

    #[test]
    fn a_dot_git_directory_is_never_provably_absent() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap();
        assert!(!git_provably_absent(t.path()));
    }

    #[test]
    fn a_husk_inside_a_repo_is_not_provably_absent() {
        // No `.git` in the husk, but its parent has one: git answers for the
        // parent repository there, so the probe must still run.
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap();
        let husk = t.path().join("sub").join("husk");
        std::fs::create_dir_all(&husk).unwrap();
        assert!(!git_provably_absent(&husk));
    }

    #[test]
    fn a_husk_under_a_bare_repo_shape_is_not_provably_absent() {
        let t = tempfile::tempdir().unwrap();
        let bare = t.path().join("bare");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir_all(bare.join("refs")).unwrap();
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let husk = bare.join("x");
        std::fs::create_dir_all(&husk).unwrap();
        assert!(!git_provably_absent(&husk));
    }
}
