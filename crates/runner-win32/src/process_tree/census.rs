//! An identity-checked census of a process's descendants — READ-ONLY.
//!
//! Plan `2026-11-09-tree-kill-tests-record-pid-reuse-strangers`. Tests that
//! check a tree kill need to know which processes ARE the tree, and a
//! parent-pid filter over a Toolhelp snapshot does not say: Windows keeps a
//! dead parent's pid in `th32ParentProcessID`, and that pid may since name the
//! test's own root. CI met exactly that — an orphaned MSVC `vctip.exe`, created
//! during the build, listed under a fixture's `cmd` — and the test then
//! demanded the reaper kill a stranger it correctly refused.
//!
//! The census admits a process by the same rule the walk does
//! ([`admits_descendant`]): listed under the root or under an already-admitted
//! member, created no earlier than that parent and no later than the
//! snapshot, and checked through a handle the census then KEEPS, so a member's
//! pid cannot be reused while the caller holds the census. It deliberately
//! does not share the walk's linking loop: an oracle built on the code it
//! checks cannot see a defect in that code.
//!
//! It never terminates anything and creates no process (the source scan in
//! `process_tree.rs`'s `admission_tests` covers this file too).

use super::{admits_descendant, ProcessTable};
use std::time::{Duration, Instant};

/// What one census found below a root.
pub struct DescendantCensus<H> {
    /// Admitted descendants, breadth-first, each held through the handle the
    /// identity check was made through.
    pub members: Vec<(u32, H)>,
    /// Listed under the root or a member but refused by the identity check:
    /// created before that parent (a reused parent pid), after the snapshot,
    /// or with an unreadable creation time.
    pub refused: Vec<u32>,
    /// Listed under the root or a member but not openable by this process,
    /// so neither admitted nor refused. Reported, never dropped.
    pub unopenable: Vec<u32>,
    /// The image name of each `refused` pid, index-aligned, read through the
    /// handle that made the check (`None` when unreadable). Diagnostic only.
    refused_images: Vec<Option<String>>,
}

impl<H> DescendantCensus<H> {
    /// The members' pids, breadth-first.
    pub fn member_pids(&self) -> Vec<u32> {
        self.members.iter().map(|(pid, _)| *pid).collect()
    }
}

impl<H> std::fmt::Debug for DescendantCensus<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let refused: Vec<String> = self
            .refused
            .iter()
            .zip(&self.refused_images)
            .map(|(pid, image)| format!("{pid} ({})", image.as_deref().unwrap_or("?")))
            .collect();
        f.debug_struct("DescendantCensus")
            .field("members", &self.member_pids())
            .field("refused", &refused)
            .field("unopenable", &self.unopenable)
            .finish()
    }
}

/// One census of the processes below `root` (pid `root_pid`); `self_pid` is
/// never classified. The arguments mirror [`walk`](super::walk).
///
/// `Err` when the root's creation time is unreadable or the snapshot fails —
/// never an empty census standing in for an unread one.
pub fn descendant_census<T: ProcessTable>(
    table: &T,
    root: &T::Handle,
    root_pid: u32,
    self_pid: u32,
) -> std::io::Result<DescendantCensus<T::Handle>> {
    let root_created = table.creation_time(root).ok_or_else(|| {
        std::io::Error::other(format!("pid {root_pid}: creation time unreadable"))
    })?;
    // Taken BEFORE the snapshot, as the walk does: a listed pid reused after
    // this instant names a process created after it, and is refused.
    let snapshot_at = table.now();
    let procs = table.snapshot()?;

    // (pid, creation time, handle): the creation time is each member's own,
    // the bound its children are checked against.
    let mut admitted: Vec<(u32, u64, T::Handle)> = Vec::new();
    let mut refused = Vec::new();
    let mut refused_images = Vec::new();
    let mut unopenable = Vec::new();
    let mut next = 0usize;
    loop {
        let (parent, parent_created) = match next {
            0 => (root_pid, root_created),
            i => match admitted.get(i - 1) {
                Some(m) => (m.0, m.1),
                None => break,
            },
        };
        next += 1;
        for &(pid, ppid) in &procs {
            if ppid != parent
                || pid == parent
                || pid == root_pid
                || pid == self_pid
                || admitted.iter().any(|m| m.0 == pid)
                || refused.contains(&pid)
                || unopenable.contains(&pid)
            {
                continue;
            }
            let Some(h) = table.open(pid) else {
                unopenable.push(pid);
                continue;
            };
            match table.creation_time(&h) {
                Some(created) if admits_descendant(created, parent_created, snapshot_at) => {
                    admitted.push((pid, created, h));
                }
                _ => {
                    refused_images.push(table.image_name(&h));
                    refused.push(pid);
                }
            }
        }
    }
    Ok(DescendantCensus {
        members: admitted.into_iter().map(|(pid, _, h)| (pid, h)).collect(),
        refused,
        unopenable,
        refused_images,
    })
}

/// Re-take [`descendant_census`] every 50 ms until it has at least `n`
/// members, for at most `within`. `Err` names the last census's members,
/// `refused` and `unopenable` on timeout, and passes a census error through.
pub fn descendant_census_reaching<T: ProcessTable>(
    table: &T,
    root: &T::Handle,
    root_pid: u32,
    self_pid: u32,
    n: usize,
    within: Duration,
) -> Result<DescendantCensus<T::Handle>, String> {
    let end = Instant::now() + within;
    loop {
        let census = descendant_census(table, root, root_pid, self_pid)
            .map_err(|e| format!("census below pid {root_pid} failed: {e}"))?;
        if census.members.len() >= n {
            return Ok(census);
        }
        if Instant::now() >= end {
            return Err(format!(
                "the census below pid {root_pid} never reached {n} member(s) within \
                 {within:?}; last: {census:?}"
            ));
        }
        drop(census);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The census against a scripted process table, on every OS.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;

    /// A minimal scripted table: rows in snapshot order, an unopenable set, a
    /// failing-snapshot switch and a `terminate` counter. `now` is fixed.
    struct Scripted {
        /// (pid, parent pid, creation time); `None` = creation time unreadable.
        rows: RefCell<Vec<(u32, u32, Option<u64>)>>,
        unopenable: HashSet<u32>,
        fail_snapshot: Cell<bool>,
        terminations: Cell<usize>,
        now: u64,
    }

    struct H {
        pid: u32,
        created: Option<u64>,
    }

    impl Scripted {
        fn new(rows: &[(u32, u32, u64)]) -> Self {
            Scripted {
                rows: RefCell::new(rows.iter().map(|&(p, pp, c)| (p, pp, Some(c))).collect()),
                unopenable: HashSet::new(),
                fail_snapshot: Cell::new(false),
                terminations: Cell::new(0),
                now: 1_000,
            }
        }
        fn root(&self, pid: u32) -> H {
            self.open(pid).expect("the scripted root")
        }
        fn census(&self, root: u32) -> std::io::Result<DescendantCensus<H>> {
            descendant_census(self, &self.root(root), root, SELF)
        }
    }

    impl ProcessTable for Scripted {
        type Handle = H;
        fn now(&self) -> u64 {
            self.now
        }
        fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>> {
            if self.fail_snapshot.get() {
                return Err(std::io::Error::other("scripted snapshot failure"));
            }
            Ok(self
                .rows
                .borrow()
                .iter()
                .map(|&(p, pp, _)| (p, pp))
                .collect())
        }
        fn open(&self, pid: u32) -> Option<H> {
            if self.unopenable.contains(&pid) {
                return None;
            }
            let created = self.rows.borrow().iter().find(|r| r.0 == pid)?.2;
            Some(H { pid, created })
        }
        fn creation_time(&self, h: &H) -> Option<u64> {
            h.created
        }
        fn terminate(&self, _h: &H) -> bool {
            self.terminations.set(self.terminations.get() + 1);
            true
        }
        fn has_exited(&self, _h: &H) -> bool {
            false
        }
        fn image_name(&self, h: &H) -> Option<String> {
            Some(format!("image-{}.exe", h.pid))
        }
    }

    const SELF: u32 = 4;
    const ROOT: u32 = 100;

    #[test]
    fn process_tree_census_of_a_childless_root_is_empty() {
        let t = Scripted::new(&[(ROOT, 1, 10), (500, 1, 20)]);
        let c = t.census(ROOT).unwrap();
        assert!(
            c.members.is_empty() && c.refused.is_empty() && c.unopenable.is_empty(),
            "{c:?}"
        );
    }

    #[test]
    fn process_tree_census_admits_children_and_grandchildren_breadth_first() {
        // The grandchild is listed first: breadth-first order, not row order.
        let t = Scripted::new(&[
            (ROOT, 1, 10),
            (300, 200, 30),
            (200, ROOT, 20),
            (250, ROOT, 25),
        ]);
        let c = t.census(ROOT).unwrap();
        assert_eq!(c.member_pids(), vec![200, 250, 300], "{c:?}");
        assert!(c.refused.is_empty() && c.unopenable.is_empty(), "{c:?}");
        assert!(
            c.members.iter().all(|(pid, h)| *pid == h.pid),
            "held through its own handle"
        );
    }

    /// The CI shape: an orphaned `vctip`, created during the build, lists the
    /// root's pid as its parent because the root reused its dead parent's pid.
    #[test]
    fn process_tree_census_refuses_a_process_older_than_the_root_it_is_listed_under() {
        let t = Scripted::new(&[(ROOT, 1, 10), (8064, ROOT, 5), (200, ROOT, 20)]);
        let c = t.census(ROOT).unwrap();
        assert_eq!(c.member_pids(), vec![200], "{c:?}");
        assert_eq!(c.refused, vec![8064], "{c:?}");
        let shown = format!("{c:?}");
        assert!(shown.contains("8064 (image-8064.exe)"), "{shown}");
    }

    /// The rule is per LINK: a process created after the root but before the
    /// member it is listed under (whose pid it outlived) is not that member's.
    #[test]
    fn process_tree_census_refuses_a_process_older_than_the_member_it_is_listed_under() {
        let t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 30), (300, 200, 20)]);
        let c = t.census(ROOT).unwrap();
        assert_eq!(c.member_pids(), vec![200], "{c:?}");
        assert_eq!(c.refused, vec![300], "{c:?}");
    }

    /// A listed pid reused after the snapshot instant names a newer process.
    #[test]
    fn process_tree_census_refuses_a_process_created_after_the_snapshot() {
        let t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 1_001)]);
        let c = t.census(ROOT).unwrap();
        assert!(c.members.is_empty(), "{c:?}");
        assert_eq!(c.refused, vec![200], "{c:?}");
    }

    #[test]
    fn process_tree_census_reports_an_unopenable_process_and_never_admits_it() {
        let mut t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 20), (300, 200, 30)]);
        t.unopenable.insert(200);
        let c = t.census(ROOT).unwrap();
        assert!(c.members.is_empty(), "{c:?}");
        assert_eq!(c.unopenable, vec![200], "{c:?}");
        assert!(c.refused.is_empty(), "{c:?}");
    }

    #[test]
    fn process_tree_census_never_classifies_the_calling_process() {
        let t = Scripted::new(&[(ROOT, 1, 10), (SELF, ROOT, 20), (200, ROOT, 20)]);
        let c = t.census(ROOT).unwrap();
        assert_eq!(c.member_pids(), vec![200], "{c:?}");
        assert!(
            !c.refused.contains(&SELF) && !c.unopenable.contains(&SELF),
            "{c:?}"
        );
    }

    #[test]
    fn process_tree_census_errs_on_a_failed_snapshot_or_an_unreadable_root() {
        let t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 20)]);
        t.fail_snapshot.set(true);
        let err = t.census(ROOT).map(|c| c.member_pids()).unwrap_err();
        assert!(
            err.to_string().contains("scripted snapshot failure"),
            "{err}"
        );

        let t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 20)]);
        t.rows.borrow_mut()[0].2 = None;
        let err = t.census(ROOT).map(|c| c.member_pids()).unwrap_err();
        assert!(
            err.to_string().contains("creation time unreadable"),
            "{err}"
        );
    }

    #[test]
    fn process_tree_census_is_repeatable_and_terminates_nothing() {
        let mut t = Scripted::new(&[
            (ROOT, 1, 10),
            (200, ROOT, 20),
            (300, 200, 30),
            (8064, ROOT, 5),
            (400, ROOT, 40),
        ]);
        t.unopenable.insert(400);
        let a = t.census(ROOT).unwrap();
        let b = t.census(ROOT).unwrap();
        assert_eq!(a.member_pids(), b.member_pids());
        assert_eq!(a.refused, b.refused);
        assert_eq!(a.unopenable, b.unopenable);
        assert_eq!(a.member_pids(), vec![200, 300]);
        assert_eq!(t.terminations.get(), 0, "a census never terminates");
    }

    #[test]
    fn process_tree_census_reaching_returns_once_reached_and_names_the_refused_on_timeout() {
        let t = Scripted::new(&[(ROOT, 1, 10), (200, ROOT, 20)]);
        let root = t.root(ROOT);
        let c =
            descendant_census_reaching(&t, &root, ROOT, SELF, 1, Duration::from_secs(5)).unwrap();
        assert_eq!(c.member_pids(), vec![200]);

        let t = Scripted::new(&[(ROOT, 1, 10), (8064, ROOT, 5)]);
        let root = t.root(ROOT);
        let err = descendant_census_reaching(&t, &root, ROOT, SELF, 1, Duration::from_millis(120))
            .map(|c| c.member_pids())
            .unwrap_err();
        assert!(
            err.contains("never reached 1") && err.contains("8064"),
            "{err}"
        );

        t.fail_snapshot.set(true);
        let err = descendant_census_reaching(&t, &root, ROOT, SELF, 1, Duration::from_secs(5))
            .map(|c| c.member_pids())
            .unwrap_err();
        assert!(err.contains("scripted snapshot failure"), "{err}");
    }
}
