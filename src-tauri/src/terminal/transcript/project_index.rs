//! Per-scan index of each Claude config dir's `projects/` directory, and the
//! pure selector that decides which `(project path, config dir)` pairs a
//! transcript scan actually needs to probe.
//!
//! **Why this exists.** The all-projects listing used to call
//! [`super::list_sessions`] for EVERY pair of (workspace child dir x Claude
//! config dir) — 744 x 18 = 13,392 pairs per scan on the operator box — and
//! each call starts with a `stat` of a directory that almost never exists.
//! At a DEBUG filter every miss also wrote a log line, which is how the
//! primary's log grew to 9-11 GB/day (plan
//! `2026-10-19-runner-transcript-scan-stats-and-logs-every-missing-project-dir`).
//!
//! Reading each config dir's `projects/` ONCE per scan (one `read_dir` per
//! config dir) and probing only the pairs whose encoded name is present makes
//! the cost scale with the directories that exist, not with workspace size x
//! account count. There is no cross-scan state: every scan re-reads the
//! index, so freshness is exactly what the exhaustive probe had.
//!
//! The selector must never lose a session the exhaustive probe would have
//! found, so it is deliberately conservative:
//!
//! 1. names are compared case-insensitively (`exists()` is case-insensitive
//!    on Windows; on Linux this can only cause a false POSITIVE, which costs
//!    the one stat `list_sessions` still does);
//! 2. an encoded name longer than Claude Code's directory-name cap is always
//!    probed, because an exact-name lookup can never prove such a pair absent
//!    (the parent's `MAX_PROJECT_DIR_NAME`);
//! 3. a `projects/` that is NotFound makes every pair in that config dir
//!    absent — exactly what `list_sessions` would conclude — while ANY other
//!    read error makes every pair in that dir probed as before, because an
//!    unreadable index is UNKNOWN, not empty;
//! 4. pairs are emitted project-major, config-dir-minor, the order the
//!    exhaustive loop used, because the caller's dedup keeps the first copy.
//!
//! The encoding is never re-implemented: [`select_pairs`] calls the parent
//! module's `encode_project_path` and reads its private
//! `MAX_PROJECT_DIR_NAME`, which is why this is a CHILD module of
//! `terminal::transcript`.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;

// Claude Code caps an encoded project-directory name at
// `MAX_PROJECT_DIR_NAME` (the parent module's constant, the one its
// prefix-scanning `project_dir` resolver uses) and disambiguates longer ones
// with a hash suffix. A name over that length cannot be proven absent by an
// exact-name lookup, so the selector always probes it and leaves resolution to
// `list_sessions`. The encoded name is pure ASCII, so its byte length, char
// count and UTF-16 length are equal, and the comparison matches
// `project_dir`'s exactly.
use super::MAX_PROJECT_DIR_NAME;

/// What one config dir's `projects/` directory looked like at scan time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectsIndex {
    /// `projects/` was read; holds the case-folded ([`fold_name`]) name of
    /// every entry in it.
    Present(HashSet<String>),
    /// `projects/` does not exist — no pair under this config dir can have
    /// sessions.
    Absent,
    /// `projects/` could not be read for a reason other than NotFound. The
    /// contents are UNKNOWN, so every pair under it is probed.
    Unreadable(String),
}

impl ProjectsIndex {
    /// Read `<config_dir>/projects` once.
    ///
    /// EVERY entry name is kept, whatever its type: `list_sessions` gates on
    /// `Path::exists`, which follows symlinks and junctions, so filtering on
    /// `DirEntry::file_type().is_dir()` (which does not) could drop a linked
    /// project dir and lose its sessions. A non-directory entry only costs
    /// the one stat `list_sessions` still does, so the index stays a SUPERSET
    /// of what `exists()` would find. An error while iterating the listing
    /// makes the whole index `Unreadable` rather than returning a partial set
    /// that would silently skip real pairs.
    pub fn read(config_dir: &Path) -> Self {
        let entries = match std::fs::read_dir(config_dir.join("projects")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return ProjectsIndex::Absent,
            Err(e) => return ProjectsIndex::Unreadable(e.to_string()),
        };
        let mut names = HashSet::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    names.insert(fold_name(&entry.file_name().to_string_lossy()));
                }
                Err(e) => return ProjectsIndex::Unreadable(e.to_string()),
            }
        }
        ProjectsIndex::Present(names)
    }
}

/// The pairs a scan should probe, plus the counts its one aggregate log line
/// reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairSelection {
    /// `(path_idx, dir_idx)` into the caller's two slices, project-major.
    pub pairs: Vec<(usize, usize)>,
    /// Pairs skipped because the index proved them absent (a `Present` index
    /// without the name, or an `Absent` index).
    pub skipped_absent: usize,
    /// Pairs probed only because their config dir's index was `Unreadable`.
    pub probed_unreadable: usize,
    /// Pairs probed only because their encoded name exceeds
    /// `MAX_PROJECT_DIR_NAME` (selected although absent from a `Present`
    /// index).
    pub probed_long_name: usize,
}

/// Case-fold a directory name for the index lookup. The encoder emits ASCII
/// only, and NTFS `exists()` is ASCII case-insensitive for such names: it does
/// NOT alias `ſ`, `ı` or the Kelvin sign to `s`, `i` or `K` (measured on an
/// NTFS volume 2026-10-24). A full-Unicode upper-case is a superset of that
/// ASCII fold, so it can only add false POSITIVES, each of which costs the one
/// stat `list_sessions` still does; the same holds on a case-sensitive
/// filesystem.
pub(crate) fn fold_name(name: &str) -> String {
    name.to_uppercase()
}

/// Decide which `(project path, config dir)` pairs need a `list_sessions`
/// probe. Pure: no filesystem access.
pub fn select_pairs(project_paths: &[String], indexes: &[ProjectsIndex]) -> PairSelection {
    let mut sel = PairSelection::default();
    for (path_idx, project) in project_paths.iter().enumerate() {
        let encoded = super::encode_project_path(project);
        let long_name = encoded.len() > MAX_PROJECT_DIR_NAME;
        let folded = fold_name(&encoded);
        for (dir_idx, index) in indexes.iter().enumerate() {
            match index {
                ProjectsIndex::Present(names) => {
                    if names.contains(&folded) {
                        sel.pairs.push((path_idx, dir_idx));
                    } else if long_name {
                        sel.pairs.push((path_idx, dir_idx));
                        sel.probed_long_name += 1;
                    } else {
                        sel.skipped_absent += 1;
                    }
                }
                ProjectsIndex::Absent => sel.skipped_absent += 1,
                ProjectsIndex::Unreadable(_) => {
                    sel.pairs.push((path_idx, dir_idx));
                    sel.probed_unreadable += 1;
                }
            }
        }
    }
    sel
}

#[cfg(test)]
mod tests {
    use super::super::tests::{clear_cache_under, write_session_with_mtime};
    use super::super::{encode_project_path, list_sessions};
    use super::*;

    fn present(names: &[&str]) -> ProjectsIndex {
        ProjectsIndex::Present(names.iter().map(|n| fold_name(n)).collect())
    }

    /// Workspace-shaped project paths `D:\ws\p00` .. `D:\ws\p{n-1}`.
    fn paths(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("D:\\ws\\p{i:02}")).collect()
    }

    #[test]
    fn zero_paths_or_zero_indexes_select_nothing() {
        assert_eq!(select_pairs(&[], &[]), PairSelection::default());
        assert_eq!(select_pairs(&paths(3), &[]), PairSelection::default());
        let none = select_pairs(&[], &[present(&["x"]), ProjectsIndex::Absent]);
        assert_eq!(none, PairSelection::default());
    }

    /// Mutation: make the `Present` arm select a missing name (or drop the
    /// `contains` check) and `pairs` becomes `[(0, 0)]` with `skipped_absent`
    /// 0 — red.
    #[test]
    fn one_path_one_index_selects_only_when_present() {
        let p = paths(1);
        let name = encode_project_path(&p[0]);
        let hit = select_pairs(&p, &[present(&[&name])]);
        assert_eq!(hit.pairs, vec![(0, 0)]);
        assert_eq!(hit.skipped_absent, 0);

        let miss = select_pairs(&p, &[present(&["something-else"])]);
        assert_eq!(miss.pairs, Vec::<(usize, usize)>::new());
        assert_eq!(miss.skipped_absent, 1);
    }

    /// Many paths x many dirs, every index variant at once.
    ///
    /// Mutation: make the `Absent` arm select its pairs and dir 1 contributes
    /// 50 extra pairs and `skipped_absent` drops to 48 — red. Make
    /// `Unreadable` skip and the 50 dir-2 pairs vanish — red. Reorder the
    /// loops (config-dir-major) and the literal order breaks — red.
    #[test]
    fn many_paths_many_dirs_select_exactly_present_and_unreadable_pairs_in_project_major_order() {
        let p = paths(50);
        let a = encode_project_path(&p[7]);
        let b = encode_project_path(&p[31]);
        let indexes = vec![
            present(&[&a, &b, "unrelated-dir"]),
            ProjectsIndex::Absent,
            ProjectsIndex::Unreadable("access denied".into()),
        ];
        let sel = select_pairs(&p, &indexes);

        let mut expected = Vec::new();
        for i in 0..50 {
            if i == 7 || i == 31 {
                expected.push((i, 0));
            }
            expected.push((i, 2));
        }
        assert_eq!(sel.pairs, expected);
        assert_eq!(sel.pairs.len(), 52);
        assert_eq!(
            sel.skipped_absent, 98,
            "48 misses in dir 0 + 50 in the Absent dir 1"
        );
        assert_eq!(sel.probed_unreadable, 50);
        assert_eq!(sel.probed_long_name, 0);
    }

    /// The on-disk dir is `d--qontinui-root-foo`; the cwd encodes to
    /// `D--qontinui-root-foo`. Both sides are folded, so the pair is selected.
    ///
    /// Mutation: drop the `fold_name` on the encoded name in `select_pairs`
    /// and the pair is skipped — red. (Dropping it on the entry names in
    /// `ProjectsIndex::read` is caught by `read_keeps_every_entry_name_case_folded`.)
    #[test]
    fn a_case_mismatch_between_cwd_and_directory_still_selects() {
        let indexes = vec![present(&["d--qontinui-root-foo"])];
        let sel = select_pairs(&["D:\\qontinui-root\\foo".to_string()], &indexes);
        assert_eq!(sel.pairs, vec![(0, 0)]);
    }

    /// Mutation: drop the `long_name` arm and the pair is skipped — red.
    #[test]
    fn a_name_over_the_cap_is_probed_even_when_absent_from_the_index() {
        let long = format!("D:\\ws\\{}", "a".repeat(MAX_PROJECT_DIR_NAME + 10));
        assert!(encode_project_path(&long).len() > MAX_PROJECT_DIR_NAME);
        let sel = select_pairs(&[long], &[present(&["unrelated"])]);
        assert_eq!(sel.pairs, vec![(0, 0)]);
        assert_eq!(sel.probed_long_name, 1);
        assert_eq!(sel.skipped_absent, 0);

        // An Absent `projects/` still proves even a long pair absent: there is
        // no directory for any resolver to find.
        let absent = select_pairs(
            &[format!("D:\\ws\\{}", "b".repeat(MAX_PROJECT_DIR_NAME + 10))],
            &[ProjectsIndex::Absent],
        );
        assert!(absent.pairs.is_empty());
    }

    #[test]
    fn read_reports_absent_when_there_is_no_projects_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(ProjectsIndex::read(temp.path()), ProjectsIndex::Absent);
    }

    /// The index is a superset of what `exists()` can find: every entry name,
    /// whatever its type, case-folded.
    ///
    /// Mutation: filter to `file_type().is_dir()` and `NOTES.TXT` drops out —
    /// red (and a symlinked/junctioned project dir would be lost the same
    /// way). Drop the fold and the set holds `Mixed-Case` — red.
    #[test]
    fn read_keeps_every_entry_name_case_folded() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(projects.join("Mixed-Case")).unwrap();
        std::fs::create_dir_all(projects.join("d--ws-p01")).unwrap();
        std::fs::write(projects.join("notes.txt"), b"x").unwrap();

        let expected: HashSet<String> = [
            "MIXED-CASE".to_string(),
            "D--WS-P01".to_string(),
            "NOTES.TXT".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            ProjectsIndex::read(temp.path()),
            ProjectsIndex::Present(expected)
        );
    }

    /// `projects` existing as a FILE is not NotFound: the index cannot be
    /// read as a directory, so it is Unreadable (probe everything), never
    /// Absent (skip everything).
    #[test]
    fn read_reports_unreadable_when_projects_is_not_a_directory() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("projects"), b"not a dir").unwrap();
        assert!(matches!(
            ProjectsIndex::read(temp.path()),
            ProjectsIndex::Unreadable(_)
        ));
    }

    /// Filesystem-backed equivalence: the selected pairs yield exactly the
    /// sessions the exhaustive (path x dir) loop yields, and both equal the
    /// fixture's literal set.
    ///
    /// Mutation: make `ProjectsIndex::read` return `Absent` for an existing
    /// `projects/`, or make `select_pairs` skip `Present` hits, and the
    /// selected set loses `s-a` / `s-b` — red.
    #[test]
    fn selected_pairs_find_exactly_the_sessions_the_exhaustive_probe_finds() {
        let temp = tempfile::tempdir().unwrap();
        let dir_a = temp.path().join("cfg-a");
        let dir_b = temp.path().join("cfg-b");
        let dir_c = temp.path().join("cfg-c");
        std::fs::create_dir_all(dir_b.join("projects")).unwrap();
        std::fs::create_dir_all(&dir_c).unwrap();

        let project_paths: Vec<String> = (0..30)
            .map(|i| {
                temp.path()
                    .join(format!("ws-proj-{i:02}"))
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        let t = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        write_session_with_mtime(&dir_a, &project_paths[4], "s-a", t);
        write_session_with_mtime(&dir_a, &project_paths[19], "s-b", t);
        clear_cache_under(temp.path());

        let dirs = [dir_a, dir_b, dir_c];
        let indexes: Vec<ProjectsIndex> = dirs.iter().map(|d| ProjectsIndex::read(d)).collect();
        assert!(matches!(indexes[0], ProjectsIndex::Present(_)));
        assert_eq!(indexes[1], ProjectsIndex::Present(HashSet::new()));
        assert_eq!(indexes[2], ProjectsIndex::Absent);

        let sel = select_pairs(&project_paths, &indexes);
        assert_eq!(sel.pairs, vec![(4, 0), (19, 0)]);

        let ids = |pairs: &mut dyn Iterator<Item = (usize, usize)>| -> HashSet<String> {
            pairs
                .flat_map(|(p, d)| list_sessions(&dirs[d], &project_paths[p]).unwrap())
                .map(|s| s.session_id)
                .collect()
        };
        let selected = ids(&mut sel.pairs.iter().copied());
        let exhaustive =
            ids(&mut (0..project_paths.len()).flat_map(|p| (0..dirs.len()).map(move |d| (p, d))));
        let expected: HashSet<String> =
            ["s-a".to_string(), "s-b".to_string()].into_iter().collect();
        assert_eq!(selected, expected);
        assert_eq!(exhaustive, expected);
    }
}
