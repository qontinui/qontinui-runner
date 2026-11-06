#![cfg(test)]

//! Tests for [`super`], the `cpu.stat` attribution, over fixture cgroup trees
//! in temporary directories. The three named fixture tests of the plan's
//! Phase 2 gate are marked GATE. The gate's fixture process table belongs to
//! the Windows arm, which waits on runner#2468; until it lands the gate is met
//! for Linux only.

use std::time::{Duration, Instant};

use super::*;

/// The user manager the fixtures' runner and panes live under.
const MGR: &str = "user.slice/user-1000.slice/user@1000.service";

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// The runner's unit where it runs today, as a systemd user service.
fn unit() -> String {
    format!("{MGR}/app.slice/qontinui-runner.service")
}

/// A holder pane's scope, named as `qontinui_pty_holder::spawn` names them.
fn holder(pane: &str) -> String {
    format!("{MGR}/app.slice/qontinui-pty-holder-{pane}-16f2c-18e4ca879535bc14.scope")
}

fn builds() -> String {
    format!("{MGR}/agent.slice/agent-builds.slice")
}

/// A `/proc/<pid>/status` body for a process running as `uid`.
fn status(uid: &str) -> String {
    format!("Name:\tqontinui-runner\nUmask:\t0002\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t1000\t1000\t1000\t1000\n")
}

/// A fixture machine: a cgroup v2 root, and a `/proc` naming the runner's
/// cgroup and, as uid 1000, its user.
struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
    procfs: PathBuf,
    /// `QONTINUI_CI_CGROUP_ROOT`, when the fixture's operator set one.
    ci_root: Option<PathBuf>,
}

impl Machine {
    fn new(runner_unit: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cgroup");
        let procfs = dir.path().join("proc");
        write(&root, "cgroup.controllers", "cpuset cpu io memory pids\n");
        if let Some(u) = runner_unit {
            write(&procfs, "self/cgroup", &format!("0::/{u}\n"));
        }
        write(&procfs, "self/status", &status("1000"));
        Self {
            _dir: dir,
            root,
            procfs,
            ci_root: None,
        }
    }

    /// Set the cumulative CPU of `rel` (`""` is the root), creating it.
    fn usage(&self, rel: &str, usec: u64) {
        let file = if rel.is_empty() {
            "cpu.stat".to_string()
        } else {
            format!("{rel}/cpu.stat")
        };
        let body = format!("usage_usec {usec}\nuser_usec {usec}\nsystem_usec 0\nnr_periods 0\n");
        write(&self.root, &file, &body);
    }

    fn usages(&self, at: &[(&str, u64)]) {
        for (rel, usec) in at {
            self.usage(rel, *usec);
        }
    }

    fn read(&self, prev: Option<&Snapshot>, now: Instant) -> (Snapshot, Option<CpuReport>) {
        self.read_counting(prev, now, DispatchCounts::default)
    }

    /// A read while `dispatches` says what `ci_node` has counted.
    fn read_counting(
        &self,
        prev: Option<&Snapshot>,
        now: Instant,
        dispatches: impl Fn() -> DispatchCounts,
    ) -> (Snapshot, Option<CpuReport>) {
        measure(
            &self.root,
            Some(&self.procfs),
            self.ci_root.as_deref(),
            &self.memory_dirs(),
            dispatches,
            prev,
            || now,
        )
    }

    fn placement(&self) -> Placement {
        Placement::find(
            &self.root,
            Some(&self.procfs),
            self.ci_root.as_deref(),
            &self.memory_dirs(),
        )
    }

    /// Where `read_slices` reads each watched slice's memory from, which
    /// `collect_from` hands to [`attach`].
    fn memory_dirs(&self) -> Vec<(&'static str, PathBuf)> {
        super::super::read_slices(&self.root).dirs
    }

    /// Two reads 30 s apart, with `first` and then `second` written before each.
    fn two_reads(&self, first: &[(&str, u64)], second: &[(&str, u64)]) -> CpuReport {
        let t0 = Instant::now();
        self.usages(first);
        let (s, r) = self.read(None, t0);
        assert_eq!(r, None, "a first read has nothing to subtract");
        self.usages(second);
        self.read(Some(&s), t0 + Duration::from_secs(30))
            .1
            .expect("a second read reports")
    }
}

fn usec(r: &CpuReport, key: &str) -> Option<u64> {
    r.attribution.get(key).and_then(Value::as_u64)
}

/// coord's measure of nesting: a scalar is depth 1, a container one more than
/// its deepest child.
fn json_depth(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

/// coord's NUL check: in any key or string value.
fn has_nul(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains('\0'),
        Value::Array(a) => a.iter().any(has_nul),
        Value::Object(m) => m.iter().any(|(k, v)| k.contains('\0') || has_nul(v)),
        _ => false,
    }
}

/// Puts a directory's mode back to `0o755` when dropped, panics included.
#[cfg(unix)]
struct RestoreMode(PathBuf);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// Makes `dir` unlistable until the guard drops: mode `--x` lets its files and
/// subdirectories be opened by name but not be listed. `None`, said on
/// stderr, where permissions are not enforced (a test run as root).
#[cfg(unix)]
fn unlistable(dir: &Path) -> Option<RestoreMode> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o100)).unwrap();
    let guard = RestoreMode(dir.to_path_buf());
    if fs::read_dir(dir).is_ok() {
        eprintln!("SKIPPED: directory permissions are not enforced here (root?)");
        return None;
    }
    Some(guard)
}

#[test]
fn usage_usec_is_read_from_its_own_line_only() {
    let stat = "usage_usec 6123468444000\nuser_usec 4521434512000\nsystem_usec 1602033932000\n";
    assert_eq!(parse_usage_usec(stat), Some(6_123_468_444_000));
    assert_eq!(
        parse_usage_usec("user_usec 5\nsystem_usec 6\n"),
        None,
        "no usage line is no reading, not 0"
    );
    assert_eq!(parse_usage_usec("usage_usec lots\n"), None);
    assert_eq!(parse_usage_usec(""), None);
}

/// A sum of CPU times saturates rather than wrapping or panicking, and a sum
/// one of whose parts was not measured is not measured either.
#[test]
fn sums_saturate_and_a_missing_part_leaves_no_sum() {
    assert_eq!(sum_usec([u64::MAX, 1]), u64::MAX);
    assert_eq!(sum_usec([2, 3]), 5);
    assert_eq!(sum_usec(Vec::<u64>::new()), 0);
    assert_eq!(sum_measured([Some(u64::MAX), Some(1)]), Some(u64::MAX));
    assert_eq!(sum_measured([Some(2), Some(3)]), Some(5));
    assert_eq!(sum_measured([Some(1), None, Some(2)]), None);
    assert_eq!(sum_measured(Vec::<Option<u64>>::new()), Some(0));
}

#[test]
fn the_runner_unit_is_the_last_unit_on_the_runners_cgroup_path() {
    let u = unit();
    assert_eq!(
        parse_runner_unit(&format!("0::/{u}\n")).as_deref(),
        Some(u.as_str())
    );
    assert_eq!(
        parse_runner_unit(
            "12:pids:/user.slice\n0::/system.slice/qontinui-runner.service/payload\n"
        )
        .as_deref(),
        Some("system.slice/qontinui-runner.service"),
        "a sub-cgroup of the unit is cut back to the unit, and v1 lines are ignored"
    );
    assert_eq!(
        parse_runner_unit("0::/user.slice/user-1000.slice/session-3.scope\n").as_deref(),
        Some("user.slice/user-1000.slice/session-3.scope")
    );
    assert_eq!(
        parse_runner_unit("0::/system.slice/qontinui-runner.service (deleted)\n").as_deref(),
        Some("system.slice/qontinui-runner.service"),
        "a removed cgroup's suffix is dropped, as ci_node drops it"
    );
    let manager_init = format!("0::/{MGR}/init.scope\n");
    let manager = format!("0::/{MGR}\n");
    for body in [
        "0::/\n",
        manager.as_str(),
        manager_init.as_str(),
        "0::/init.scope\n",
        "0::/ci.slice\n",
        "0::/user.slice/../x.service\n",
        "",
    ] {
        assert_eq!(parse_runner_unit(body), None, "{body:?}");
    }
}

#[test]
fn holders_are_looked_for_under_the_runners_user_manager() {
    assert_eq!(runner_manager(&unit()).as_deref(), Some(MGR));
    assert_eq!(
        runner_manager("user.slice/user-1000.slice/session-3.scope").as_deref(),
        Some(MGR),
        "a runner in a login session: its user's manager"
    );
    assert_eq!(runner_manager("system.slice/qontinui-runner.service"), None);
    assert_eq!(runner_manager("user.slice/user-abc.slice/x.scope"), None);
    assert_eq!(uid_manager(&status("1000")).as_deref(), Some(MGR));
    assert_eq!(
        uid_manager("Uid:\t1000\t0\t0\t0\n").as_deref(),
        Some(MGR),
        "the real uid, not the effective one"
    );
    for body in [
        "Name:\tx\n",
        "Uid:\n",
        "Uid:\tabc\t1\t1\t1\n",
        "Uid:\t-1\n",
        "",
    ] {
        assert_eq!(uid_manager(body), None, "{body:?}");
    }

    let d = tempfile::tempdir().unwrap();
    let m = d.path().join(MGR);
    for rel in [
        "qontinui-pty-holder-a-1-1.scope",
        "app.slice/qontinui-pty-holder-b-1-1.scope",
        "background.slice/qontinui-pty-holder-c-1-1.scope",
        "app.slice/qontinui-runner.service",
        "app.slice/vte-spawn-1.scope",
        "app.slice/qontinui-pty-holder-d-1-1.service",
    ] {
        write(&m, &format!("{rel}/cpu.stat"), "usage_usec 1\n");
    }
    let found: Vec<String> = holder_scopes(&m)
        .expect("every directory lists")
        .iter()
        .map(|p| rel_of(&m, p).unwrap())
        .collect();
    assert_eq!(
        found,
        [
            "app.slice/qontinui-pty-holder-b-1-1.scope",
            "background.slice/qontinui-pty-holder-c-1-1.scope",
            "qontinui-pty-holder-a-1-1.scope",
        ]
    );
}

#[test]
fn a_delta_needs_the_same_cgroup_and_a_counter_that_did_not_go_backwards() {
    let at = |id, usage_usec| Some(Reading { id, usage_usec });
    assert_eq!(delta(at(7, 100), at(7, 250)), Some(150));
    assert_eq!(
        delta(at(7, 100), at(7, 100)),
        Some(0),
        "an idle cgroup is a real 0"
    );
    assert_eq!(delta(None, at(7, 250)), None, "nothing to subtract from");
    assert_eq!(delta(at(7, 100), None), None);
    assert_eq!(
        delta(at(7, 100), at(8, 250)),
        None,
        "recreated: a new counter"
    );
    assert_eq!(delta(at(7, 250), at(7, 100)), None, "reset: never negative");
}

#[test]
fn the_first_read_of_a_root_publishes_no_cpu() {
    let u = unit();
    let m = Machine::new(Some(&u));
    m.usages(&[("", 1_000), ("user.slice", 500), (u.as_str(), 100)]);
    let before = json!({"ci-runners.slice": {"memory_current": 1}});
    let mut slices = before.as_object().unwrap().clone();
    let pressure = m.procfs.join("pressure");
    attach(
        &m.root,
        &pressure,
        &m.memory_dirs(),
        DispatchCounts::default,
        &mut slices,
    );
    assert_eq!(
        Value::Object(slices.clone()),
        before,
        "a first read adds nothing: no 0, no empty object"
    );
    std::thread::sleep(Duration::from_millis(2));
    m.usages(&[("", 2_000), ("user.slice", 900), (u.as_str(), 300)]);
    attach(
        &m.root,
        &pressure,
        &m.memory_dirs(),
        DispatchCounts::default,
        &mut slices,
    );
    assert_eq!(slices[ATTRIBUTION_KEY]["runner_unit_usec"], json!(200));
    assert!(slices[ATTRIBUTION_KEY]["interval_usec"].as_u64().unwrap() > 0);
}

/// Previous reads are kept for at most `MAX_ROOTS` roots. A new root past that
/// takes the place of the one read longest ago, which is neither the first
/// root kept nor the first by name here; a root already kept is replaced in
/// place and takes no other root's.
#[test]
fn a_new_root_past_the_bound_replaces_the_one_read_longest_ago() {
    let m = Machine::new(Some(&unit()));
    m.usages(&[("", 0)]);
    let t0 = Instant::now();
    let (s, _) = m.read(None, t0);
    let read_at = |ms: usize| Snapshot {
        taken_at: t0 + Duration::from_millis(ms as u64),
        ..s.clone()
    };
    let root = |i: usize| PathBuf::from(format!("/fixture/root-{i}"));
    let mut map = BTreeMap::new();
    // root-0 is read last and root-15 first.
    for i in 0..MAX_ROOTS {
        keep(&mut map, &root(i), read_at(MAX_ROOTS - i));
    }
    let oldest = root(MAX_ROOTS - 1);

    keep(&mut map, &root(3), read_at(100));
    assert_eq!(map.len(), MAX_ROOTS, "a root read again");
    assert!(
        map.contains_key(&oldest),
        "a root read again takes no place"
    );
    assert_eq!(map[&root(3)].taken_at, t0 + Duration::from_millis(100));

    keep(&mut map, &root(MAX_ROOTS), read_at(200));
    let kept: BTreeSet<PathBuf> = map.keys().cloned().collect();
    let want: BTreeSet<PathBuf> = (0..=MAX_ROOTS)
        .filter(|&i| i != MAX_ROOTS - 1)
        .map(root)
        .collect();
    assert_eq!(kept, want, "the root read longest ago made way");
}

/// GATE: a named slice nested in `user.slice` or `system.slice` leaves that
/// remainder exactly once. A slice whose time also stayed in the remainder
/// breaks the sums checked last.
#[test]
fn a_named_slice_inside_user_or_system_slice_leaves_its_remainder_exactly_once() {
    let (u, b) = (unit(), builds());
    let m = Machine::new(Some(&u));
    write(&m.root, &format!("{b}/memory.current"), "4096\n");
    let runners = "system.slice/ci-runners.slice";
    let r = m.two_reads(
        &[
            ("", 9_000_000_000),
            ("user.slice", 5_000_000_000),
            ("system.slice", 2_000_000_000),
            (u.as_str(), 700_000_000),
            (b.as_str(), 900_000_000),
            (runners, 300_000_000),
        ],
        &[
            ("", 9_100_000_000),
            ("user.slice", 5_040_000_000),
            ("system.slice", 2_012_000_000),
            (u.as_str(), 704_000_000),
            (b.as_str(), 930_000_000),
            (runners, 307_000_000),
        ],
    );
    assert_eq!(usec(&r, "session_usec"), Some(34_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(6_000_000));
    assert_eq!(usec(&r, "ci_usec"), Some(7_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(5_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(59_000_000));
    assert_eq!(r.slices.get("agent-builds.slice"), Some(&30_000_000));
    assert_eq!(r.slices.get("ci-runners.slice"), Some(&7_000_000));
    // Each slice's time is in its class or in the remainder, never in both.
    assert_eq!(
        usec(&r, "session_usec").unwrap() + usec(&r, "user_slice_remainder_usec").unwrap(),
        40_000_000
    );
    assert_eq!(
        usec(&r, "ci_usec").unwrap() + usec(&r, "system_slice_remainder_usec").unwrap(),
        12_000_000
    );
}

/// GATE: with no `qontinui-runner.slice`, the holder scopes beside the
/// runner's unit in `app.slice` are session work.
#[test]
fn holder_scopes_in_app_slice_with_no_runner_slice_are_session_work() {
    let (u, a, b) = (unit(), holder("a"), holder("b"));
    let other_user =
        "user.slice/user-1001.slice/user@1001.service/app.slice/qontinui-pty-holder-z-1-1.scope";
    let terminal = format!("{MGR}/app.slice/vte-spawn-1.scope");
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            (u.as_str(), 0),
            (a.as_str(), 0),
            (b.as_str(), 0),
            (other_user, 0),
            (terminal.as_str(), 0),
        ],
        &[
            ("", 90_000_000),
            ("user.slice", 60_000_000),
            (u.as_str(), 5_000_000),
            (a.as_str(), 20_000_000),
            (b.as_str(), 15_000_000),
            (other_user, 7_000_000),
            (terminal.as_str(), 3_000_000),
        ],
    );
    let placed = m.placement();
    assert!(
        placed
            .watched
            .iter()
            .all(|(slice, _)| *slice != "qontinui-runner.slice")
            && placed.session_slices.is_empty(),
        "no qontinui-runner.slice anywhere the resolver looks"
    );
    assert_eq!(
        usec(&r, "session_usec"),
        Some(40_000_000),
        "the unit and both holders"
    );
    assert_eq!(usec(&r, "pty_holder_scopes_usec"), Some(35_000_000));
    assert_eq!(r.attribution["pty_holder_scopes_measured"], json!(2));
    assert_eq!(
        r.attribution["pty_holder_scopes_seen"],
        json!(2),
        "another user's holder is not this runner's"
    );
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
    assert_eq!(r.attribution["runner_unit"], json!(u));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(20_000_000));
    assert!(
        r.attribution.get("ci_usec").is_none(),
        "no CI cgroup here: no CI total, not 0"
    );
}

/// GATE: `ci-winvm.slice` under `ci.slice` is CI work. `ci.slice`'s whole time
/// is the CI total, so a child with no delta yet leaves it measured. The
/// runner runs in the root cgroup here: it has no unit, and `ci_node` would
/// make no dispatch cgroup beside it.
#[test]
fn ci_winvm_slice_under_ci_slice_is_charged_to_ci() {
    let m = Machine::new(Some(""));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            ("system.slice", 0),
            ("ci.slice", 0),
            ("ci.slice/ci-runners.slice", 0),
            ("ci.slice/ci-winvm.slice", 0),
        ],
        &[
            ("", 100_000_000),
            ("user.slice", 30_000_000),
            ("system.slice", 10_000_000),
            ("ci.slice", 50_000_000),
            ("ci.slice/ci-runners.slice", 20_000_000),
            ("ci.slice/ci-winvm.slice", 25_000_000),
        ],
    );
    assert_eq!(usec(&r, "ci_usec"), Some(50_000_000));
    assert_eq!(
        r.attribution["ci_children_usec"],
        json!({"ci-runners.slice": 20_000_000, "ci-winvm.slice": 25_000_000})
    );
    assert_eq!(usec(&r, "unattributed_usec"), Some(50_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(30_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(10_000_000));
    assert!(r.attribution.get("session_usec").is_none());
    assert_eq!(
        r.attribution["ci_children_unmeasured"],
        json!(0),
        "both children measured: a real 0"
    );
    assert_eq!(r.attribution["ci_children_omitted"], json!(0));
    assert!(r.attribution.get("ci_children_unlisted").is_none());
    assert_eq!(r.slices.get("ci-runners.slice"), Some(&20_000_000));

    // The VM's slice appeared since the last read.
    let m = Machine::new(Some(""));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("ci.slice", 0), ("ci.slice/ci-runners.slice", 0)]);
    let (s, _) = m.read(None, t0);
    m.usages(&[
        ("", 80_000_000),
        ("ci.slice", 45_000_000),
        ("ci.slice/ci-runners.slice", 20_000_000),
        ("ci.slice/ci-winvm.slice", 25_000_000),
    ]);
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    assert_eq!(usec(&r, "ci_usec"), Some(45_000_000));
    assert_eq!(
        r.attribution["ci_children_usec"],
        json!({"ci-runners.slice": 20_000_000})
    );
    assert_eq!(
        r.attribution["ci_children_unmeasured"],
        json!(1),
        "the VM's slice is named but has no delta yet, so the children are partial"
    );
}

#[test]
fn a_runner_unit_inside_qontinui_runner_slice_is_not_charged_twice() {
    let slice = format!("{MGR}/qontinui.slice/qontinui-runner.slice");
    let u = format!("{slice}/qontinui-runner.service");
    let m = Machine::new(Some(&u));
    write(&m.root, &format!("{slice}/memory.current"), "1\n");
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            (slice.as_str(), 0),
            (u.as_str(), 0),
        ],
        &[
            ("", 50_000_000),
            ("user.slice", 30_000_000),
            (slice.as_str(), 10_000_000),
            (u.as_str(), 8_000_000),
        ],
    );
    assert_eq!(usec(&r, "session_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "runner_unit_usec"), Some(8_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(20_000_000));
    assert_eq!(r.slices.get("qontinui-runner.slice"), Some(&10_000_000));
}

#[test]
fn holders_come_and_go_without_withholding_session_work() {
    let (u, a, b) = (unit(), holder("a"), holder("new"));
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("user.slice", 0), (u.as_str(), 0), (a.as_str(), 0)]);
    let (s1, _) = m.read(None, t0);
    m.usages(&[
        ("", 60_000_000),
        ("user.slice", 40_000_000),
        (u.as_str(), 5_000_000),
        (a.as_str(), 10_000_000),
        (b.as_str(), 9_000_000),
    ]);
    let (s2, r) = m.read(Some(&s1), t0 + Duration::from_secs(30));
    let r = r.unwrap();
    assert_eq!(
        usec(&r, "session_usec"),
        Some(15_000_000),
        "the new holder has no delta yet"
    );
    assert_eq!(r.attribution["pty_holder_scopes_measured"], json!(1));
    assert_eq!(r.attribution["pty_holder_scopes_seen"], json!(2));
    assert_eq!(r.attribution["pty_holder_scopes_ended"], json!(0));
    assert_eq!(
        usec(&r, "user_slice_remainder_usec"),
        Some(25_000_000),
        "its time is in the remainder, which is where it was spent"
    );
    assert_eq!(
        usec(&r, "session_usec").unwrap() + usec(&r, "user_slice_remainder_usec").unwrap(),
        40_000_000
    );

    // From its second read on it is session work.
    m.usages(&[
        ("", 120_000_000),
        ("user.slice", 80_000_000),
        (u.as_str(), 10_000_000),
        (a.as_str(), 20_000_000),
        (b.as_str(), 18_000_000),
    ]);
    let (s3, r) = m.read(Some(&s2), t0 + Duration::from_secs(60));
    let r = r.unwrap();
    assert_eq!(usec(&r, "session_usec"), Some(24_000_000));
    assert_eq!(r.attribution["pty_holder_scopes_measured"], json!(2));

    // A pane that ends withholds nothing: its time stays in user.slice, and
    // the count of ended panes says that some of it was session work.
    fs::remove_dir_all(m.root.join(&b)).unwrap();
    m.usages(&[
        ("", 180_000_000),
        ("user.slice", 120_000_000),
        (u.as_str(), 15_000_000),
        (a.as_str(), 30_000_000),
    ]);
    let r = m.read(Some(&s3), t0 + Duration::from_secs(90)).1.unwrap();
    assert_eq!(usec(&r, "session_usec"), Some(15_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(25_000_000));
    assert_eq!(r.attribution["pty_holder_scopes_seen"], json!(1));
    assert_eq!(r.attribution["pty_holder_scopes_ended"], json!(1));
}

/// The count of ended panes compares this read's holders with the previous
/// read's, so a previous read that did not find them all leaves it out rather
/// than counting none.
#[test]
fn no_ended_pane_count_after_a_read_that_did_not_find_every_holder() {
    let (u, a) = (unit(), holder("a"));
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("user.slice", 0), (u.as_str(), 0), (a.as_str(), 0)]);
    let (mut s, _) = m.read(None, t0);
    assert!(s.holders.is_some(), "this read found them all");
    // What a read that could not list where they live keeps.
    s.holders = None;
    m.usages(&[
        ("", 60_000_000),
        ("user.slice", 40_000_000),
        (u.as_str(), 5_000_000),
        (a.as_str(), 10_000_000),
    ]);
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    assert_eq!(r.attribution["pty_holder_scopes_seen"], json!(1));
    assert_eq!(r.attribution["pty_holder_scopes_measured"], json!(1));
    assert!(
        r.attribution.get("pty_holder_scopes_ended").is_none(),
        "unknown, not 0"
    );
    assert_eq!(
        usec(&r, "session_usec"),
        Some(15_000_000),
        "this read found them all"
    );
}

#[test]
fn a_counter_that_went_backwards_gives_no_delta_and_withholds_what_needs_it() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[
        ("", 1_000_000_000),
        ("user.slice", 500_000_000),
        (u.as_str(), 300_000_000),
        ("ci.slice", 0),
    ]);
    let (s1, _) = m.read(None, t0);
    m.usages(&[
        ("", 1_100_000_000),
        ("user.slice", 540_000_000),
        (u.as_str(), 2_000_000),
        ("ci.slice", 9_000_000),
    ]);
    let (s2, r) = m.read(Some(&s1), t0 + Duration::from_secs(30));
    let r = r.unwrap();
    for key in [
        "runner_unit_usec",
        "session_usec",
        "user_slice_remainder_usec",
        "unattributed_usec",
    ] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key} needs the reset counter"
        );
    }
    assert_eq!(usec(&r, "host_usec"), Some(100_000_000));
    assert_eq!(usec(&r, "ci_usec"), Some(9_000_000), "CI does not need it");

    // The read after it has a baseline again.
    m.usages(&[
        ("", 1_200_000_000),
        ("user.slice", 580_000_000),
        (u.as_str(), 6_000_000),
        ("ci.slice", 18_000_000),
    ]);
    let r = m.read(Some(&s2), t0 + Duration::from_secs(60)).1.unwrap();
    assert_eq!(usec(&r, "runner_unit_usec"), Some(4_000_000));
    assert_eq!(usec(&r, "session_usec"), Some(4_000_000));
}

/// The identity check, not the counter: the recreated slice's counter is
/// higher than the old one's, which a counter check alone would accept.
#[cfg(unix)]
#[test]
fn a_recreated_cgroup_gives_no_delta_even_when_its_counter_is_higher() {
    let (u, b) = (unit(), builds());
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        (u.as_str(), 0),
        (b.as_str(), 100),
    ]);
    let (s1, _) = m.read(None, t0);
    // The old directory still exists while the new one is made, so the new one
    // cannot reuse its inode.
    let (dir, old) = (m.root.join(&b), m.root.join(format!("{b}.old")));
    fs::rename(&dir, &old).unwrap();
    m.usages(&[
        ("", 50_000_000),
        ("user.slice", 40_000_000),
        (u.as_str(), 5_000_000),
        (b.as_str(), 30_000_000),
    ]);
    fs::remove_dir_all(&old).unwrap();
    let r = m.read(Some(&s1), t0 + Duration::from_secs(30)).1.unwrap();
    assert!(
        !r.slices.contains_key("agent-builds.slice"),
        "a new directory is a new counter"
    );
    assert!(r.attribution.get("session_usec").is_none());
    assert!(r.attribution.get("user_slice_remainder_usec").is_none());
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
}

#[test]
fn a_named_slice_gone_since_the_last_read_withholds_its_class_and_remainder_once() {
    let (u, b) = (unit(), builds());
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        (u.as_str(), 0),
        (b.as_str(), 0),
        ("ci.slice", 0),
    ]);
    let (s1, _) = m.read(None, t0);
    fs::remove_dir_all(m.root.join(&b)).unwrap();
    m.usages(&[
        ("", 60_000_000),
        ("user.slice", 40_000_000),
        (u.as_str(), 5_000_000),
        ("ci.slice", 9_000_000),
    ]);
    let (s2, r) = m.read(Some(&s1), t0 + Duration::from_secs(30));
    let r = r.unwrap();
    for key in [
        "session_usec",
        "user_slice_remainder_usec",
        "unattributed_usec",
    ] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: the gone slice's last time is in user.slice and in no named cgroup"
        );
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
    assert_eq!(usec(&r, "ci_usec"), Some(9_000_000));
    assert_eq!(usec(&r, "host_usec"), Some(60_000_000));
    assert_eq!(r.attribution["withheld"], json!(["cgroup_gone"]));

    m.usages(&[
        ("", 120_000_000),
        ("user.slice", 80_000_000),
        (u.as_str(), 10_000_000),
        ("ci.slice", 18_000_000),
    ]);
    let r = m.read(Some(&s2), t0 + Duration::from_secs(60)).1.unwrap();
    assert_eq!(usec(&r, "session_usec"), Some(5_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(35_000_000));
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");
}

/// A named cgroup gone since the previous read whose last time is in a named
/// cgroup of its own class withholds nothing: `ci-runners.slice` stopped
/// inside `ci.slice`, and a dispatch cgroup ended under an operator's
/// `QONTINUI_CI_CGROUP_ROOT` inside `ci.slice`, are CI work there.
#[test]
fn a_named_cgroup_gone_from_inside_one_of_its_own_class_withholds_nothing() {
    let u = unit();
    let mut m = Machine::new(Some(&u));
    m.ci_root = Some(m.root.join("ci.slice/dispatches"));
    let runners = "ci.slice/ci-runners.slice";
    let job = "ci.slice/dispatches/dispatch-j1-0123abcd";
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("system.slice", 10_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    let counts = || DispatchCounts {
        started: 1,
        ..DispatchCounts::default()
    };
    m.usages(&at(0));
    m.usages(&[(runners, 0), (job, 0)]);
    let (s, _) = m.read_counting(None, t0, counts);
    assert_eq!(s.strict.get(runners), Some(&Class::Ci));
    assert_eq!(s.strict.get(job), Some(&Class::Ci));

    fs::remove_dir_all(m.root.join(runners)).unwrap();
    fs::remove_dir_all(m.root.join(job)).unwrap();
    m.usages(&at(1));
    let r = m
        .read_counting(Some(&s), t0 + Duration::from_secs(30), counts)
        .1
        .unwrap();
    assert_eq!(usec(&r, "ci_usec"), Some(20_000_000));
    assert_eq!(usec(&r, "session_usec"), Some(50_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(30_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(10_000_000));
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");
}

#[test]
fn nested_time_past_its_parent_clamps_within_read_skew_and_is_withheld_beyond_it() {
    assert_eq!(exclusive("t", 100, 40), Some(60));
    assert_eq!(exclusive("t", 0, SKEW_FLOOR_USEC), Some(0));
    assert_eq!(exclusive("t", 0, SKEW_FLOOR_USEC + 1), None);
    assert_eq!(
        exclusive("t", 10_000_000_000, 10_150_000_000),
        Some(0),
        "1.5% of a large parent is skew"
    );
    assert_eq!(exclusive("t", 10_000_000_000, 10_300_000_000), None);

    let u = unit();
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[("", 0), ("user.slice", 0), (u.as_str(), 0)],
        &[
            ("", 30_000_000),
            ("user.slice", 10_000_000),
            (u.as_str(), 10_050_000),
        ],
    );
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(0));

    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[("", 0), ("user.slice", 0), (u.as_str(), 0)],
        &[
            ("", 30_000_000),
            ("user.slice", 10_000_000),
            (u.as_str(), 12_000_000),
        ],
    );
    assert!(r.attribution.get("user_slice_remainder_usec").is_none());
    assert!(
        r.attribution.get("withheld").is_none(),
        "an absence past read skew names no reason"
    );
    assert_eq!(usec(&r, "session_usec"), Some(12_000_000));
}

/// A root that cannot be listed leaves `unattributed_usec` absent with no
/// reason listed: without the sum of the root's children, the root's own
/// counter, which can read below them, would stand for the host's time. The
/// class totals and the remainders need no listing of the root and are
/// published.
#[cfg(unix)]
#[test]
fn an_unlistable_root_publishes_no_unattributed_time_and_names_no_reason() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("user.slice", 0), (u.as_str(), 0)]);
    let (s, _) = m.read(None, t0);
    m.usages(&[
        ("", 30_000_000),
        ("user.slice", 10_000_000),
        (u.as_str(), 4_000_000),
    ]);
    let Some(_listing) = unlistable(&m.root) else {
        return;
    };
    let r = m
        .read(Some(&s), t0 + Duration::from_secs(30))
        .1
        .expect("a second read reports");
    assert_eq!(usec(&r, "host_usec"), Some(30_000_000));
    assert_eq!(usec(&r, "session_usec"), Some(4_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(6_000_000));
    assert!(
        r.attribution.get("unattributed_usec").is_none(),
        "no listing of the root, no host time to take the named cgroups from"
    );
    assert!(
        r.attribution.get("withheld").is_none(),
        "an absence no reason covers"
    );
}

/// The runner runs in the root cgroup, so it has no unit.
#[test]
fn a_machine_with_no_named_cgroup_publishes_its_host_time_as_unattributed() {
    let m = Machine::new(Some(""));
    let r = m.two_reads(
        &[("", 0), ("user.slice", 0), ("system.slice", 0)],
        &[
            ("", 40_000_000),
            ("user.slice", 25_000_000),
            ("system.slice", 5_000_000),
        ],
    );
    assert!(r.attribution.get("session_usec").is_none());
    assert!(r.attribution.get("ci_usec").is_none());
    assert!(r.attribution.get("runner_unit").is_none());
    assert_eq!(usec(&r, "unattributed_usec"), Some(40_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(25_000_000));
    assert_eq!(
        usec(&r, "pty_holder_scopes_usec"),
        Some(0),
        "no holder scopes: a real 0"
    );
}

#[test]
fn a_runner_inside_a_holder_scope_is_counted_once() {
    let (own, pane) = (holder("runner"), holder("pane"));
    let m = Machine::new(Some(&own));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            (own.as_str(), 0),
            (pane.as_str(), 0),
        ],
        &[
            ("", 50_000_000),
            ("user.slice", 30_000_000),
            (own.as_str(), 6_000_000),
            (pane.as_str(), 4_000_000),
        ],
    );
    assert_eq!(usec(&r, "session_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "runner_unit_usec"), Some(6_000_000));
    assert_eq!(usec(&r, "pty_holder_scopes_usec"), Some(4_000_000));
    assert_eq!(
        r.attribution["pty_holder_scopes_seen"],
        json!(1),
        "the runner's own scope is its unit, not a pane"
    );
}

#[test]
fn reads_with_no_time_between_them_report_nothing() {
    let m = Machine::new(None);
    m.usages(&[("", 5)]);
    let t0 = Instant::now();
    let (s, _) = m.read(None, t0);
    assert_eq!(m.read(Some(&s), t0).1, None);
}

/// A read after a gap past the stale line publishes nothing, and the next read
/// measures from it rather than across the gap.
#[test]
fn a_read_after_a_gap_past_the_stale_line_publishes_nothing_and_becomes_the_baseline() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("user.slice", 0), (u.as_str(), 0)]);
    let (s0, _) = m.read(None, t0);

    m.usages(&[
        ("", 40_000_000),
        ("user.slice", 30_000_000),
        (u.as_str(), 9_000_000),
    ]);
    let at_line = t0 + Duration::from_micros(MAX_INTERVAL_USEC);
    let r = m
        .read(Some(&s0), at_line)
        .1
        .expect("a read at the line reports");
    assert_eq!(r.attribution["interval_usec"], json!(MAX_INTERVAL_USEC));
    assert_eq!(usec(&r, "runner_unit_usec"), Some(9_000_000));

    let late = t0 + Duration::from_micros(MAX_INTERVAL_USEC + 1);
    let (s1, r) = m.read(Some(&s0), late);
    assert_eq!(r, None, "a delta past the stale line is not published");

    m.usages(&[
        ("", 70_000_000),
        ("user.slice", 50_000_000),
        (u.as_str(), 10_000_000),
    ]);
    let r = m
        .read(Some(&s1), late + Duration::from_secs(30))
        .1
        .expect("the late read is the next one's baseline");
    assert_eq!(r.attribution["interval_usec"], json!(30_000_000));
    assert_eq!(usec(&r, "runner_unit_usec"), Some(1_000_000));
    assert_eq!(usec(&r, "host_usec"), Some(30_000_000));
}

#[test]
fn a_name_is_measured_as_written_into_the_json() {
    assert_eq!(
        published_len("ci-runners.slice"),
        "ci-runners.slice".len() + 2
    );
    assert_eq!(published_len("a\"b"), 6);
    assert_eq!(
        published_len("\u{1}"),
        8,
        "a control byte is written as \\u0001"
    );
    let raw = format!("ci-{}.slice", "\u{1}".repeat(200));
    assert!(raw.len() <= MAX_PUBLISHED_NAME_BYTES);
    assert!(published_len(&raw) > MAX_PUBLISHED_NAME_BYTES);
}

/// Names a cgroup can legally carry, built to escape to the published-name cap
/// or just past it, keep the document inside coord's caps.
#[cfg(unix)]
#[test]
fn names_that_grow_when_escaped_keep_the_document_inside_coords_caps() {
    // `ci-` + 83 control bytes + 3 digits + `.slice` escapes to exactly the cap.
    let fits = |i: usize| format!("ci-{}{i:03}.slice", "\u{1}".repeat(83));
    let past = |i: usize| format!("ci-{}{i:03}.slice", "\u{1}".repeat(84));
    assert_eq!(published_len(&fits(0)), MAX_PUBLISHED_NAME_BYTES);
    assert_eq!(published_len(&past(0)), MAX_PUBLISHED_NAME_BYTES + 6);
    let u = format!("{MGR}/app.slice/{}.service", "\u{1}".repeat(80));
    assert!(published_len(&u) > MAX_PUBLISHED_NAME_BYTES);

    let m = Machine::new(Some(&u));
    // Each cgroup's time in the second read; the first reads 0 everywhere.
    let mut cgroups: Vec<(String, u64)> = vec![
        (String::new(), 3_000_000_000),
        ("user.slice".into(), 1_000_000_000),
        ("ci.slice".into(), 1_000_000_000),
        (u.clone(), 1_000_000_000),
    ];
    cgroups.extend((0..20).map(|i| (format!("ci.slice/{}", fits(i)), 40_000_000)));
    cgroups.extend((0..5).map(|i| (format!("ci.slice/{}", past(i)), 40_000_000)));
    let first: Vec<(&str, u64)> = cgroups.iter().map(|(c, _)| (c.as_str(), 0)).collect();
    let second: Vec<(&str, u64)> = cgroups.iter().map(|(c, v)| (c.as_str(), *v)).collect();
    let r = m.two_reads(&first, &second);

    let mut slices = Map::new();
    apply(&r, &mut slices);
    let doc = Value::Object(slices);
    let bytes = serde_json::to_vec(&doc).unwrap();
    assert!(bytes.len() <= 16 * 1024, "{} bytes", bytes.len());
    assert!(doc.as_object().unwrap().len() <= 16);
    assert!(json_depth(&doc) <= 8);
    assert!(!has_nul(&doc));
    let a = &doc[ATTRIBUTION_KEY];
    let named = a["ci_children_usec"].as_object().unwrap();
    assert_eq!(named.len(), MAX_CI_CHILDREN);
    assert!(named
        .keys()
        .all(|k| published_len(k) <= MAX_PUBLISHED_NAME_BYTES));
    assert_eq!(
        a["ci_children_omitted"],
        json!(5 + 20 - MAX_CI_CHILDREN),
        "the names past the cap, and the children past the count"
    );
    assert!(
        a.get("runner_unit").is_none(),
        "a name past the cap is left out"
    );
    assert_eq!(
        usec(&r, "runner_unit_usec"),
        Some(1_000_000_000),
        "its time is not"
    );
}

/// A runner that is a system service has no user manager on its cgroup path:
/// its panes are looked for under its own user's manager, and never under
/// another user's.
#[test]
fn a_runner_outside_a_user_manager_finds_the_holders_of_its_own_user() {
    let u = "system.slice/qontinui-runner.service";
    let (a, b) = (holder("a"), holder("b"));
    let other_user =
        "user.slice/user-1001.slice/user@1001.service/app.slice/qontinui-pty-holder-z-1-1.scope";
    let first = [
        ("", 0),
        ("user.slice", 0),
        ("system.slice", 0),
        (u, 0),
        (a.as_str(), 0),
        (b.as_str(), 0),
        (other_user, 0),
    ];
    let second = [
        ("", 90_000_000),
        ("user.slice", 60_000_000),
        ("system.slice", 20_000_000),
        (u, 5_000_000),
        (a.as_str(), 20_000_000),
        (b.as_str(), 15_000_000),
        (other_user, 7_000_000),
    ];

    let m = Machine::new(Some(u));
    let r = m.two_reads(&first, &second);
    assert_eq!(r.attribution["pty_holder_scopes_seen"], json!(2));
    assert_eq!(usec(&r, "pty_holder_scopes_usec"), Some(35_000_000));
    assert_eq!(
        usec(&r, "session_usec"),
        Some(40_000_000),
        "the unit and uid 1000's holders"
    );
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(25_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(15_000_000));

    let m = Machine::new(Some(u));
    fs::remove_file(m.procfs.join("self/status")).unwrap();
    let r = m.two_reads(&first, &second);
    for key in [
        "pty_holder_scopes_seen",
        "pty_holder_scopes_measured",
        "pty_holder_scopes_usec",
        "session_usec",
    ] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: no user manager known, so no holder was looked for"
        );
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
}

/// The root's counter comes from the kernel's per-CPU accounting, and its
/// children's from their tasks' run time, so the root can read a little below
/// them (1.3% here). `unattributed_usec` then stands on the children's sum, and
/// never reads below what they hold outside the named cgroups, both remainders
/// included.
#[test]
fn unattributed_time_is_never_below_what_the_roots_children_hold_outside_named_cgroups() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            ("system.slice", 0),
            ("init.scope", 0),
            (u.as_str(), 0),
        ],
        &[
            ("", 98_700_000),
            ("user.slice", 60_000_000),
            ("system.slice", 39_000_000),
            ("init.scope", 1_000_000),
            (u.as_str(), 20_000_000),
        ],
    );
    assert_eq!(
        usec(&r, "host_usec"),
        Some(98_700_000),
        "the root's own counter, as read"
    );
    let user = usec(&r, "user_slice_remainder_usec").unwrap();
    let system = usec(&r, "system_slice_remainder_usec").unwrap();
    assert_eq!((user, system), (40_000_000, 39_000_000));
    assert_eq!(
        usec(&r, "unattributed_usec"),
        Some(80_000_000),
        "the children's 100 s less the runner's 20 s, not the root's 98.7 s less it"
    );
    assert!(usec(&r, "unattributed_usec").unwrap() >= user + system);
    assert_eq!(usec(&r, "session_usec"), Some(20_000_000));

    // Where the root reads above its children, the kernel time it shows stays.
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            ("system.slice", 0),
            (u.as_str(), 0),
        ],
        &[
            ("", 110_000_000),
            ("user.slice", 60_000_000),
            ("system.slice", 40_000_000),
            (u.as_str(), 20_000_000),
        ],
    );
    assert_eq!(usec(&r, "unattributed_usec"), Some(90_000_000));
}

/// A CI job `ci_node` contains runs in a `dispatch-*` cgroup inside the
/// runner's unit (with `DelegateSubgroup=`, the runner itself sits in a
/// sub-cgroup of the unit). Its time is CI work and not session work, and a
/// dispatch that starts or ends withholds both totals for that read.
#[test]
fn a_ci_dispatch_cgroup_in_the_runners_unit_is_ci_work() {
    use crate::ci_node::cgroup::dispatch_dir_name;
    let u = unit();
    let own = format!("{u}/main");
    let (job, job2) = (
        format!("{u}/{}", dispatch_dir_name("job-1")),
        format!("{u}/{}", dispatch_dir_name("job-2")),
    );
    let m = Machine::new(Some(&own));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        ("ci.slice", 0),
        (u.as_str(), 0),
        (own.as_str(), 0),
        (job.as_str(), 0),
    ]);
    let (s1, _) = m.read(None, t0);
    m.usages(&[
        ("", 100_000_000),
        ("user.slice", 60_000_000),
        ("ci.slice", 5_000_000),
        (u.as_str(), 50_000_000),
        (own.as_str(), 10_000_000),
        (job.as_str(), 40_000_000),
    ]);
    let (s2, r) = m.read(Some(&s1), t0 + Duration::from_secs(30));
    let r = r.unwrap();
    assert_eq!(
        r.attribution["runner_unit"],
        json!(u),
        "the unit, not its sub-cgroup"
    );
    assert_eq!(
        usec(&r, "runner_unit_usec"),
        Some(50_000_000),
        "the unit's own counter, the dispatch in it included"
    );
    assert_eq!(
        usec(&r, "session_usec"),
        Some(10_000_000),
        "the unit less the dispatch"
    );
    assert_eq!(
        usec(&r, "ci_usec"),
        Some(45_000_000),
        "ci.slice and the dispatch"
    );
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(45_000_000));
    assert_eq!(
        usec(&r, "session_usec").unwrap()
            + usec(&r, "ci_usec").unwrap()
            + usec(&r, "unattributed_usec").unwrap(),
        100_000_000,
        "the dispatch's time is in one class only"
    );

    // A second job starts: neither total can be split this read.
    m.usages(&[
        ("", 200_000_000),
        ("user.slice", 120_000_000),
        ("ci.slice", 10_000_000),
        (u.as_str(), 100_000_000),
        (own.as_str(), 20_000_000),
        (job.as_str(), 70_000_000),
        (job2.as_str(), 10_000_000),
    ]);
    let (s3, r) = m.read(Some(&s2), t0 + Duration::from_secs(60));
    let r = r.unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(r.attribution.get(key).is_none(), "{key}: a new dispatch");
    }
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(45_000_000));

    // It ends before the next read: its time up to then is in the unit's
    // counter and cannot be split from it.
    fs::remove_dir_all(m.root.join(&job2)).unwrap();
    m.usages(&[
        ("", 300_000_000),
        ("user.slice", 180_000_000),
        ("ci.slice", 15_000_000),
        (u.as_str(), 150_000_000),
        (own.as_str(), 30_000_000),
        (job.as_str(), 100_000_000),
    ]);
    let (s4, r) = m.read(Some(&s3), t0 + Duration::from_secs(90));
    let r = r.unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(r.attribution.get(key).is_none(), "{key}: a dispatch gone");
    }
    assert_eq!(r.attribution["withheld"], json!(["cgroup_gone"]));

    // The read after that splits again.
    m.usages(&[
        ("", 400_000_000),
        ("user.slice", 240_000_000),
        ("ci.slice", 20_000_000),
        (u.as_str(), 200_000_000),
        (own.as_str(), 40_000_000),
        (job.as_str(), 130_000_000),
    ]);
    let r = m.read(Some(&s4), t0 + Duration::from_secs(120)).1.unwrap();
    assert_eq!(usec(&r, "session_usec"), Some(20_000_000));
    assert_eq!(usec(&r, "ci_usec"), Some(35_000_000));

    // Without `DelegateSubgroup=` the runner is in the unit itself, and a
    // dispatch cgroup there is found the same way.
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            (u.as_str(), 0),
            (job.as_str(), 0),
        ],
        &[
            ("", 50_000_000),
            ("user.slice", 30_000_000),
            (u.as_str(), 20_000_000),
            (job.as_str(), 15_000_000),
        ],
    );
    assert_eq!(usec(&r, "session_usec"), Some(5_000_000));
    assert_eq!(usec(&r, "ci_usec"), Some(15_000_000));
}

/// `ci_node` puts every dispatch cgroup in the directory an operator names in
/// `QONTINUI_CI_CGROUP_ROOT`, and looks nowhere else. When that directory is
/// under the cgroup root its dispatches are CI work; one outside it, or named
/// through `..`, is not followed, and nothing a dispatch could be part of is
/// published.
#[test]
fn a_dispatch_cgroup_under_the_operators_ci_cgroup_root_is_ci_work() {
    use crate::ci_node::cgroup::dispatch_dir_name;
    let u = unit();
    let parent = format!("{MGR}/ci-dispatch.slice");
    let job = format!("{parent}/{}", dispatch_dir_name("job-1"));
    let first = [
        ("", 0),
        ("user.slice", 0),
        (u.as_str(), 0),
        (parent.as_str(), 0),
        (job.as_str(), 0),
    ];
    let second = [
        ("", 100_000_000),
        ("user.slice", 70_000_000),
        (u.as_str(), 10_000_000),
        (parent.as_str(), 40_000_000),
        (job.as_str(), 40_000_000),
    ];

    // With no override the job is not in the runner's unit, so it is not
    // found: its time is in the `user.slice` remainder, where it was spent.
    let m = Machine::new(Some(&u));
    let r = m.two_reads(&first, &second);
    assert_eq!(usec(&r, "session_usec"), Some(10_000_000));
    assert!(r.attribution.get("ci_usec").is_none(), "no CI cgroup found");
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(60_000_000));

    let mut m = Machine::new(Some(&u));
    m.ci_root = Some(m.root.join(&parent));
    let r = m.two_reads(&first, &second);
    assert_eq!(m.placement().dispatches, Some(vec![job.clone()]));
    assert_eq!(usec(&r, "ci_usec"), Some(40_000_000));
    assert_eq!(usec(&r, "session_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(20_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(50_000_000));
    assert_eq!(
        usec(&r, "session_usec").unwrap()
            + usec(&r, "ci_usec").unwrap()
            + usec(&r, "unattributed_usec").unwrap(),
        100_000_000,
        "the job's time is in one class only"
    );

    // An override outside the cgroup root, holding a dispatch cgroup of its
    // own, and one that reaches the job's directory through `..`. Neither is
    // followed, and `ci_node` makes dispatch cgroups nowhere else, so one could
    // be anywhere: nothing it could be part of is published.
    let mut m = Machine::new(Some(&u));
    let elsewhere = m._dir.path().join("elsewhere");
    write(
        &elsewhere,
        &format!("{}/cpu.stat", dispatch_dir_name("job-1")),
        "usage_usec 0\n",
    );
    for ci_root in [
        elsewhere,
        m.root.join(format!("{MGR}/app.slice/../ci-dispatch.slice")),
    ] {
        m.ci_root = Some(ci_root);
        assert_eq!(
            m.placement().dispatches,
            Some(Vec::new()),
            "{:?} is not followed",
            m.ci_root
        );
        let r = m.two_reads(&first, &second);
        for key in [
            "session_usec",
            "ci_usec",
            "unattributed_usec",
            "user_slice_remainder_usec",
        ] {
            assert!(
                r.attribution.get(key).is_none(),
                "{key}: {:?} is not followed",
                m.ci_root
            );
        }
        assert_eq!(usec(&r, "host_usec"), Some(100_000_000));
        assert_eq!(usec(&r, "runner_unit_usec"), Some(10_000_000));
        assert_eq!(
            r.attribution["withheld"],
            json!(["ci_cgroup_root_not_followed"]),
            "{:?}",
            m.ci_root
        );
    }
}

/// A dispatch that started and ended between two reads is in neither listing,
/// and its time stays in the cgroup it was made in. `ci_node` counts the
/// dispatches it starts, and a read across which that count moved withholds
/// what such a dispatch's time would be part of. With the dispatch parents in
/// the runner's unit, that is the two class totals. An operator's
/// `QONTINUI_CI_CGROUP_ROOT` outside every named cgroup also takes
/// `unattributed_usec` and its slice's remainder; one inside `ci.slice` takes
/// nothing more.
#[test]
fn a_dispatch_no_read_listed_withholds_what_its_time_is_part_of() {
    let u = unit();
    let first = [
        ("", 0),
        ("user.slice", 0),
        ("system.slice", 0),
        ("ci.slice", 0),
        (u.as_str(), 0),
    ];
    let second = [
        ("", 100_000_000),
        ("user.slice", 60_000_000),
        ("system.slice", 10_000_000),
        ("ci.slice", 20_000_000),
        (u.as_str(), 50_000_000),
    ];
    let started = |n| DispatchCounts {
        started: n,
        ..DispatchCounts::default()
    };
    let reads = |m: &Machine, before, after| {
        let t0 = Instant::now();
        m.usages(&first);
        let (s, _) = m.read_counting(None, t0, move || before);
        m.usages(&second);
        m.read_counting(Some(&s), t0 + Duration::from_secs(30), move || after)
            .1
            .expect("a second read reports")
    };
    let absent = |r: &CpuReport, keys: &[&str], why: &str| {
        for key in keys {
            assert!(r.attribution.get(*key).is_none(), "{key}: {why}");
        }
    };

    let started_why = json!(["dispatch_started"]);
    let mut m = Machine::new(Some(&u));
    let r = reads(&m, started(3), started(3));
    assert_eq!(
        usec(&r, "session_usec"),
        Some(50_000_000),
        "no new dispatch"
    );
    assert_eq!(usec(&r, "ci_usec"), Some(20_000_000));
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");

    // The unit's 50 s may hold a dispatch's time.
    let r = reads(&m, started(3), started(4));
    absent(&r, &["session_usec", "ci_usec"], "an unlisted dispatch");
    assert_eq!(r.attribution["withheld"], started_why);
    assert_eq!(usec(&r, "runner_unit_usec"), Some(50_000_000));
    assert_eq!(
        usec(&r, "unattributed_usec"),
        Some(30_000_000),
        "the unit is named either way"
    );
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(10_000_000));

    m.ci_root = Some(m.root.join(format!("{MGR}/ci-dispatch.slice")));
    let r = reads(&m, started(3), started(4));
    absent(
        &r,
        &[
            "session_usec",
            "ci_usec",
            "unattributed_usec",
            "user_slice_remainder_usec",
        ],
        "a dispatch made outside every named cgroup",
    );
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(r.attribution["withheld"], started_why);

    m.ci_root = Some(m.root.join("ci.slice/dispatches"));
    let r = reads(&m, started(3), started(4));
    absent(&r, &["session_usec", "ci_usec"], "the unit's time");
    assert_eq!(usec(&r, "unattributed_usec"), Some(30_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(r.attribution["withheld"], started_why);
}

/// A CI job `ci_node` could not contain runs in the runner's own cgroup, so
/// while one runs, and for the read after it ends, neither class total can be
/// split from the unit's time.
#[test]
fn an_uncontained_dispatch_withholds_both_totals_while_it_runs_and_for_the_read_after() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    let counts = |uncontained_running| DispatchCounts {
        started: 1,
        uncontained_running,
        uncontained_begun: 1,
    };
    m.usages(&at(0));
    let (mut s, _) = m.read_counting(None, t0, move || counts(1));
    for (k, running, split) in [(1, 1, false), (2, 0, false), (3, 0, true)] {
        m.usages(&at(k));
        let (next, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30 * k), move || {
            counts(running)
        });
        let r = r.expect("a later read reports");
        for key in ["session_usec", "ci_usec"] {
            assert_eq!(r.attribution.contains_key(key), split, "read {k}: {key}");
        }
        assert_eq!(
            usec(&r, "unattributed_usec"),
            Some(30_000_000),
            "read {k}: the job runs in a named cgroup"
        );
        let why = (!split).then(|| json!(["dispatch_uncontained"]));
        assert_eq!(r.attribution.get("withheld"), why.as_ref(), "read {k}");
        s = next;
    }
}

/// A dispatch made after a read listed the cgroups and before it read their
/// counters is in neither listing, and its first steps may be in the unit's
/// counter. `ci_node`'s count, taken again once the counters are read, shows
/// it: that read and the next withhold both totals.
#[test]
fn a_dispatch_counted_while_a_read_is_under_way_withholds_that_read_and_the_next() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    // `ci_node` counts a dispatch between the two counts of the second read.
    let calls = std::cell::Cell::new(0);
    let counts = || {
        let n = calls.get();
        calls.set(n + 1);
        DispatchCounts {
            started: u64::from(n >= 3),
            ..DispatchCounts::default()
        }
    };
    m.usages(&at(0));
    let (mut s, _) = m.read_counting(None, t0, counts);
    for (k, split) in [(1, false), (2, false), (3, true)] {
        m.usages(&at(k));
        let (next, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30 * k), counts);
        let r = r.expect("a later read reports");
        for key in ["session_usec", "ci_usec"] {
            assert_eq!(r.attribution.contains_key(key), split, "read {k}: {key}");
        }
        let why = (!split).then(|| json!(["dispatch_started"]));
        assert_eq!(r.attribution.get("withheld"), why.as_ref(), "read {k}");
        s = next;
    }
    assert_eq!(calls.get(), 8, "each read counts twice");
}

/// A read takes its first count before it lists the cgroups, and `ci_node`
/// counts a dispatch only once its cgroup exists. So a dispatch that first
/// count holds is in that read's listing, and when its cgroup is gone by the
/// next read, its last time, now in the runner's unit, withholds that read's
/// split. Were the count taken after the listing, such a dispatch would be in
/// neither listing yet counted by both reads, and that time would be
/// published as session work.
#[test]
fn a_dispatch_a_reads_first_count_holds_is_in_that_reads_listing() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let job = format!("{u}/dispatch-j1-0123abcd");
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    let one = DispatchCounts {
        started: 1,
        ..DispatchCounts::default()
    };
    m.usages(&at(0));
    let (s, _) = m.read_counting(None, t0, DispatchCounts::default);

    // `ci_node` makes the job's cgroup and counts it as the second read
    // takes its first count.
    let calls = std::cell::Cell::new(0);
    let counts = || {
        if calls.get() == 0 {
            m.usage(&job, 0);
        }
        calls.set(calls.get() + 1);
        one
    };
    m.usages(&at(1));
    let (s, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30), counts);
    assert_eq!(calls.get(), 2);
    assert_eq!(
        s.strict.get(&job),
        Some(&Class::Ci),
        "the read listed the job"
    );
    assert_eq!(
        r.unwrap().attribution["withheld"],
        json!(["dispatch_started"])
    );

    // The job ends before the next read.
    fs::remove_dir_all(m.root.join(&job)).unwrap();
    m.usages(&at(2));
    let r = m
        .read_counting(Some(&s), t0 + Duration::from_secs(60), move || one)
        .1
        .unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: the job's last time"
        );
    }
    assert_eq!(r.attribution["withheld"], json!(["cgroup_gone"]));
}

/// A read takes its second count once it has read the counters, and
/// `ci_node` runs a dispatch's steps only once it is counted. So what a step
/// uses after that count is in the next read's interval, which the count
/// withholds. Were the count taken before the counters, a dispatch counted
/// just after it, whose step then ran in the runner's unit before the
/// counters were read, would be counted by neither of that read's counts,
/// and its step's time would be published as session work.
#[test]
fn a_reads_second_count_comes_after_its_counters() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    m.usages(&at(0));
    let (s, _) = m.read_counting(None, t0, DispatchCounts::default);

    // Just after the second read's second count, a dispatch `ci_node` could
    // not contain is counted and runs a 7 s step in the runner's unit.
    let calls = std::cell::Cell::new(0);
    let counts = || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            m.usages(&[
                ("", 107_000_000),
                ("user.slice", 67_000_000),
                (u.as_str(), 57_000_000),
            ]);
        }
        DispatchCounts::default()
    };
    m.usages(&at(1));
    let (s, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30), counts);
    assert_eq!(calls.get(), 2);
    assert_eq!(
        s.readings.get(&u).map(|r| r.usage_usec),
        Some(50_000_000),
        "the counters were read before the step ran"
    );
    let r = r.unwrap();
    assert_eq!(usec(&r, "session_usec"), Some(50_000_000));
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");

    // The next read counts the dispatch, running, and withholds the step's
    // time.
    let running = DispatchCounts {
        started: 1,
        uncontained_running: 1,
        uncontained_begun: 1,
    };
    m.usages(&at(2));
    let r = m
        .read_counting(Some(&s), t0 + Duration::from_secs(60), move || running)
        .1
        .unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(r.attribution.get(key).is_none(), "{key}: the step's time");
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(50_000_000));
    assert_eq!(
        r.attribution["withheld"],
        json!(["dispatch_started", "dispatch_uncontained"])
    );
}

/// A contained job a step of which could not be moved into its cgroup runs
/// that step in the runner's unit, beside session work, from that step until
/// the job ends. The read after it began, each read while it runs and the read
/// after it ends withhold both totals, though no dispatch started meanwhile
/// and none ran uncontained when the first of those reads' previous read
/// counted.
#[test]
fn a_step_that_could_not_be_placed_withholds_both_totals_until_the_read_after_its_job() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    let counts = |uncontained_running, uncontained_begun| DispatchCounts {
        started: 1,
        uncontained_running,
        uncontained_begun,
    };
    m.usages(&at(0));
    let (mut s, _) = m.read_counting(None, t0, move || counts(0, 0));
    // The job's steps are placed, then one is not, the job runs on, it ends.
    for (k, running, begun, split) in [
        (1, 0, 0, true),
        (2, 1, 1, false),
        (3, 1, 1, false),
        (4, 0, 1, false),
        (5, 0, 1, true),
    ] {
        m.usages(&at(k));
        let (next, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30 * k), move || {
            counts(running, begun)
        });
        let r = r.expect("a later read reports");
        for key in ["session_usec", "ci_usec"] {
            assert_eq!(r.attribution.contains_key(key), split, "read {k}: {key}");
        }
        assert_eq!(
            usec(&r, "unattributed_usec"),
            Some(30_000_000),
            "read {k}: the step runs in a named cgroup"
        );
        let why = (!split).then(|| json!(["dispatch_uncontained"]));
        assert_eq!(r.attribution.get("withheld"), why.as_ref(), "read {k}");
        s = next;
    }
}

/// A step that could not be placed can begin after one read and its job can
/// end before the next, so that neither read counts it running: only the
/// count of uncontained runs begun shows that it ran. The job's cgroup is in
/// `ci.slice` here, so its going withholds nothing by itself.
#[test]
fn a_step_that_could_not_be_placed_withholds_both_totals_when_over_by_the_next_read() {
    let u = unit();
    let mut m = Machine::new(Some(&u));
    m.ci_root = Some(m.root.join("ci.slice/dispatches"));
    let job = "ci.slice/dispatches/dispatch-j1-0123abcd";
    let t0 = Instant::now();
    let at = |k: u64| {
        [
            ("", 100_000_000 * k),
            ("user.slice", 60_000_000 * k),
            ("ci.slice", 20_000_000 * k),
            (u.as_str(), 50_000_000 * k),
        ]
    };
    let counts = |uncontained_begun| DispatchCounts {
        started: 1,
        uncontained_running: 0,
        uncontained_begun,
    };
    let split = |r: &CpuReport| (usec(r, "session_usec"), usec(r, "ci_usec"));
    let published = (Some(50_000_000), Some(20_000_000));

    m.usages(&at(0));
    m.usage(job, 0);
    let (s, _) = m.read_counting(None, t0, move || counts(0));
    m.usages(&at(1));
    m.usage(job, 5_000_000);
    let (s, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(30), move || counts(0));
    assert_eq!(split(&r.unwrap()), published, "the job's steps were placed");

    fs::remove_dir_all(m.root.join(job)).unwrap();
    m.usages(&at(2));
    let (s, r) = m.read_counting(Some(&s), t0 + Duration::from_secs(60), move || counts(1));
    let r = r.unwrap();
    assert_eq!(
        split(&r),
        (None, None),
        "a step ran in the unit, and is over"
    );
    assert_eq!(
        usec(&r, "unattributed_usec"),
        Some(30_000_000),
        "the step ran in a named cgroup"
    );
    assert_eq!(
        r.attribution["withheld"],
        json!(["dispatch_uncontained"]),
        "the job's cgroup went inside ci.slice"
    );

    m.usages(&at(3));
    let r = m.read_counting(Some(&s), t0 + Duration::from_secs(90), move || counts(1));
    assert_eq!(split(&r.1.unwrap()), published, "the job is over");
}

/// With the runner in the root cgroup, `ci_node` makes no dispatch cgroup
/// beside it, and a job it could not contain runs in the root's own time,
/// which only the host's time holds: an unlisted dispatch withholds the CI
/// total and `unattributed_usec`, and leaves the remainders. With the runner's
/// own cgroup unknown, a dispatch cgroup beside it could be anywhere, so every
/// value one could be part of is absent, whether `ci_node` counted one or not;
/// under an operator's `QONTINUI_CI_CGROUP_ROOT`, only while one is counted.
#[test]
fn an_unlisted_dispatch_beside_a_runner_in_the_root_cgroup_or_an_unknown_one() {
    let first = [
        ("", 0),
        ("user.slice", 0),
        ("system.slice", 0),
        ("ci.slice", 0),
    ];
    let second = [
        ("", 100_000_000),
        ("user.slice", 60_000_000),
        ("system.slice", 10_000_000),
        ("ci.slice", 20_000_000),
    ];
    let started = |n| DispatchCounts {
        started: n,
        ..DispatchCounts::default()
    };
    let reads = |m: &Machine, after: DispatchCounts| {
        let t0 = Instant::now();
        m.usages(&first);
        let (s, _) = m.read_counting(None, t0, move || started(3));
        m.usages(&second);
        m.read_counting(Some(&s), t0 + Duration::from_secs(30), move || after)
            .1
            .expect("a second read reports")
    };
    let published = |r: &CpuReport| {
        [
            "ci_usec",
            "unattributed_usec",
            "user_slice_remainder_usec",
            "system_slice_remainder_usec",
        ]
        .map(|key| r.attribution.contains_key(key))
    };

    let in_root = Machine::new(Some(""));
    let r = reads(&in_root, started(3));
    assert_eq!(published(&r), [true; 4], "no dispatch");
    assert_eq!(usec(&r, "unattributed_usec"), Some(80_000_000));
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");
    let r = reads(&in_root, started(4));
    assert_eq!(
        published(&r),
        [false, false, true, true],
        "a dispatch in the root's own time"
    );
    assert_eq!(usec(&r, "host_usec"), Some(100_000_000));
    assert_eq!(r.attribution["withheld"], json!(["dispatch_started"]));

    let unknown = Machine::new(None);
    for (after, why) in [
        (started(3), json!(["own_cgroup_unknown"])),
        (
            started(4),
            json!(["dispatch_started", "own_cgroup_unknown"]),
        ),
    ] {
        let r = reads(&unknown, after);
        assert_eq!(published(&r), [false; 4], "{after:?}");
        assert_eq!(usec(&r, "host_usec"), Some(100_000_000));
        assert_eq!(r.attribution["withheld"], why, "{after:?}");
    }

    // With an operator's `QONTINUI_CI_CGROUP_ROOT` the dispatch cgroups are
    // all there, but a job `ci_node` could not contain still runs in the
    // runner's own cgroup, wherever that is.
    let mut unknown = Machine::new(None);
    unknown.ci_root = Some(unknown.root.join("ci.slice/dispatches"));
    let r = reads(&unknown, started(3));
    assert_eq!(published(&r), [true; 4], "no dispatch");
    assert!(r.attribution.get("withheld").is_none(), "nothing withheld");
    let r = reads(&unknown, started(4));
    assert_eq!(published(&r), [false; 4], "a dispatch");
    assert_eq!(
        r.attribution["withheld"],
        json!(["dispatch_started", "own_cgroup_unknown"])
    );
}

/// The root's own counter can read below the sum of its children, so host time
/// stands on that sum where it is larger, and a root child with no delta this
/// read (one that just appeared) leaves no sum to stand on: `unattributed_usec`
/// is absent, not the root's counter less the named cgroups.
#[test]
fn a_root_child_with_no_delta_leaves_unattributed_absent() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let r = m.two_reads(
        &[("", 0), ("user.slice", 0), (u.as_str(), 0)],
        &[
            ("", 100_000_000),
            ("user.slice", 60_000_000),
            (u.as_str(), 50_000_000),
            ("machine.slice", 45_000_000),
        ],
    );
    assert_eq!(usec(&r, "host_usec"), Some(100_000_000));
    assert!(
        r.attribution.get("unattributed_usec").is_none(),
        "machine.slice has no delta"
    );
    assert_eq!(usec(&r, "session_usec"), Some(50_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
}

/// A directory that could not be listed ([`unlistable`]) is unknown, not
/// empty.
#[cfg(unix)]
#[test]
fn a_directory_that_could_not_be_listed_is_not_taken_for_an_empty_one() {
    assert_eq!(
        child_dirs(&std::env::temp_dir().join("no-such-cgroup-directory-here")),
        Some(Vec::new()),
        "a directory that does not exist holds nothing"
    );
    let (u, a) = (unit(), holder("a"));
    let first = [("", 0), ("user.slice", 0), (u.as_str(), 0), (a.as_str(), 0)];
    let second = [
        ("", 60_000_000),
        ("user.slice", 40_000_000),
        (u.as_str(), 5_000_000),
        (a.as_str(), 10_000_000),
    ];

    // The holders' `app.slice`: they are unknown, so is session work.
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&first);
    let (s, _) = m.read(None, t0);
    m.usages(&second);
    let Some(restore) = unlistable(&m.root.join(MGR).join("app.slice")) else {
        return;
    };
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    for key in [
        "session_usec",
        "pty_holder_scopes_usec",
        "pty_holder_scopes_measured",
        "pty_holder_scopes_seen",
        "withheld",
    ] {
        assert!(r.attribution.get(key).is_none(), "{key}: holders unknown");
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
    assert_eq!(
        usec(&r, "user_slice_remainder_usec"),
        Some(35_000_000),
        "the holder's time is where it was spent"
    );
    drop(restore);

    // The runner's unit, where dispatch cgroups would be: neither class can
    // be split from it.
    let m = Machine::new(Some(&u));
    write(&m.root, "ci.slice/cpu.stat", "usage_usec 0\n");
    let t0 = Instant::now();
    m.usages(&first);
    let (s, _) = m.read(None, t0);
    m.usages(&second);
    m.usage("ci.slice", 3_000_000);
    let Some(restore) = unlistable(&m.root.join(&u)) else {
        return;
    };
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: dispatches unknown"
        );
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
    assert_eq!(r.attribution["pty_holder_scopes_seen"], json!(1));
    assert_eq!(r.attribution["withheld"], json!(["dispatch_dir_unlisted"]));
    drop(restore);

    // `ci.slice`: its own time is still the CI total, and its children are
    // marked as not listed rather than counted as none.
    let m = Machine::new(Some(&u));
    let runners = "ci.slice/ci-runners.slice";
    let t0 = Instant::now();
    m.usages(&first);
    m.usages(&[("ci.slice", 0), (runners, 0)]);
    let (s, _) = m.read(None, t0);
    m.usages(&second);
    m.usages(&[("ci.slice", 9_000_000), (runners, 4_000_000)]);
    let Some(_restore) = unlistable(&m.root.join("ci.slice")) else {
        return;
    };
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    assert_eq!(usec(&r, "ci_usec"), Some(9_000_000));
    assert_eq!(r.attribution["ci_children_unlisted"], json!(true));
    for key in [
        "ci_children_usec",
        "ci_children_unmeasured",
        "ci_children_omitted",
    ] {
        assert!(r.attribution.get(key).is_none(), "{key}: not listed");
    }
    assert_eq!(
        r.slices.get("ci-runners.slice"),
        Some(&4_000_000),
        "a watched slice is found by name, not by listing"
    );
}

/// A dispatch cgroup the previous read listed, in a directory this read could
/// not list, is withheld as unlisted, not reported gone: it may still be
/// there, as it is here.
#[cfg(unix)]
#[test]
fn a_dispatch_cgroup_in_a_directory_that_could_not_be_listed_is_unlisted_not_gone() {
    use crate::ci_node::cgroup::dispatch_dir_name;
    let u = unit();
    let own = format!("{u}/main");
    let job = format!("{u}/{}", dispatch_dir_name("job-1"));
    let m = Machine::new(Some(&own));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        ("ci.slice", 0),
        (u.as_str(), 0),
        (own.as_str(), 0),
        (job.as_str(), 0),
    ]);
    let (s, _) = m.read(None, t0);
    m.usages(&[
        ("", 100_000_000),
        ("user.slice", 60_000_000),
        ("ci.slice", 5_000_000),
        (u.as_str(), 50_000_000),
        (own.as_str(), 10_000_000),
        (job.as_str(), 40_000_000),
    ]);
    let Some(_restore) = unlistable(&m.root.join(&u)) else {
        return;
    };
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    for key in ["session_usec", "ci_usec"] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: dispatches unknown"
        );
    }
    assert_eq!(
        r.attribution["withheld"],
        json!(["dispatch_dir_unlisted"]),
        "the dispatch cgroup is still there"
    );
    assert_eq!(usec(&r, "runner_unit_usec"), Some(50_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(45_000_000));
}

/// A subdirectory whose name is not UTF-8 cannot be named in a path here, so
/// the listing it is in is unknown: here the holder scopes, and with them
/// session work.
#[cfg(target_os = "linux")]
#[test]
fn a_name_that_is_not_utf8_leaves_its_listing_unknown() {
    use std::os::unix::ffi::OsStrExt;
    let u = unit();
    let m = Machine::new(Some(&u));
    let first = [("", 0), ("user.slice", 0), (u.as_str(), 0)];
    m.usages(&first);
    let app = m.root.join(MGR).join("app.slice");
    let odd = app.join(std::ffi::OsStr::from_bytes(
        b"qontinui-pty-holder-\xff.scope",
    ));
    if let Err(e) = fs::create_dir(&odd) {
        eprintln!("SKIPPED: this filesystem refuses a name that is not UTF-8: {e}");
        return;
    }
    assert_eq!(child_dirs(&app), None);
    let r = m.two_reads(
        &first,
        &[
            ("", 30_000_000),
            ("user.slice", 20_000_000),
            (u.as_str(), 5_000_000),
        ],
    );
    for key in ["session_usec", "pty_holder_scopes_seen"] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: the holders are unknown"
        );
    }
    assert_eq!(usec(&r, "runner_unit_usec"), Some(5_000_000));
}

/// A session-class slice under another user's manager is that user's work,
/// not this runner's: it keeps its own `cpu` object, and its time is in the
/// `user.slice` remainder.
#[test]
fn another_users_agent_builds_slice_is_not_this_runners_session_work() {
    let u = unit();
    let theirs = "user.slice/user-1001.slice/user@1001.service/agent.slice/agent-builds.slice";
    let m = Machine::new(Some(&u));
    write(&m.root, &format!("{theirs}/memory.current"), "1\n");
    let r = m.two_reads(
        &[("", 0), ("user.slice", 0), (u.as_str(), 0), (theirs, 0)],
        &[
            ("", 50_000_000),
            ("user.slice", 40_000_000),
            (u.as_str(), 5_000_000),
            (theirs, 25_000_000),
        ],
    );
    assert_eq!(usec(&r, "session_usec"), Some(5_000_000));
    assert_eq!(usec(&r, "user_slice_remainder_usec"), Some(35_000_000));
    assert_eq!(
        r.slices.get("agent-builds.slice"),
        Some(&25_000_000),
        "its own delta is still published, beside its memory"
    );
}

/// The CPU read takes each watched slice's directory from the memory read and
/// does not look for it again, so a slice's `cpu` object is of the cgroup its
/// memory entry describes even when another candidate would be found first by
/// the time the CPU is read: here the other user's `agent-builds.slice`, whose
/// memory became readable in between.
#[test]
fn a_slices_cpu_is_read_where_its_memory_was_though_another_candidate_appears() {
    let (u, mine) = (unit(), builds());
    let theirs = "user.slice/user-100.slice/user@100.service/agent.slice/agent-builds.slice";
    let m = Machine::new(Some(&u));
    m.usages(&[(theirs, 0), (mine.as_str(), 0)]);
    write(&m.root, &format!("{mine}/memory.current"), "1\n");
    let memory_dirs = m.memory_dirs();
    assert_eq!(
        memory_dirs,
        vec![("agent-builds.slice", m.root.join(&mine))]
    );
    write(&m.root, &format!("{theirs}/memory.current"), "1\n");
    let placed = Placement::find(&m.root, Some(&m.procfs), None, &memory_dirs);
    assert_eq!(
        placed.watched,
        vec![("agent-builds.slice", mine.clone())],
        "the directory the memory entry was read from, not the one now found first"
    );
    assert_eq!(
        m.placement().watched,
        vec![("agent-builds.slice", theirs.to_string())],
        "a read that starts now finds the other user's first, for its memory and its cpu"
    );
}

/// The memory entry describes a slice's first candidate, which can be another
/// user's (`user-100` sorts before `user-1000`). The runner's own
/// `agent-builds.slice` behind it is still session work, while the slice's
/// `cpu` object stays of the cgroup its memory entry describes.
#[test]
fn the_runners_own_agent_builds_slice_is_found_behind_another_users() {
    let (u, mine) = (unit(), builds());
    let theirs = "user.slice/user-100.slice/user@100.service/agent.slice/agent-builds.slice";
    let m = Machine::new(Some(&u));
    for slice in [theirs, mine.as_str()] {
        write(&m.root, &format!("{slice}/memory.current"), "1\n");
    }
    let r = m.two_reads(
        &[
            ("", 0),
            ("user.slice", 0),
            (u.as_str(), 0),
            (theirs, 0),
            (mine.as_str(), 0),
        ],
        &[
            ("", 100_000_000),
            ("user.slice", 70_000_000),
            (u.as_str(), 5_000_000),
            (theirs, 25_000_000),
            (mine.as_str(), 30_000_000),
        ],
    );
    let placed = m.placement();
    assert!(
        placed
            .watched
            .contains(&("agent-builds.slice", theirs.to_string())),
        "the memory entry is the other user's"
    );
    assert_eq!(placed.session_slices, vec![mine.clone()]);
    assert_eq!(
        r.slices.get("agent-builds.slice"),
        Some(&25_000_000),
        "the cpu object is of the cgroup the memory entry describes"
    );
    assert_eq!(
        usec(&r, "session_usec"),
        Some(35_000_000),
        "the unit and the runner's own agent-builds.slice"
    );
    assert_eq!(
        usec(&r, "user_slice_remainder_usec"),
        Some(35_000_000),
        "the other user's slice is in the remainder"
    );
    assert_eq!(usec(&r, "unattributed_usec"), Some(65_000_000));
}

/// The runner's own session-class slices are looked for by name, under the
/// root, `system.slice` and its own user manager, so a `user.slice` that could
/// not be listed (a transient `EMFILE`; mode `--x` here) still leaves its
/// `agent-builds.slice` in session work, not a session total without it. No
/// CI slice was found, and one could be in a user manager that was not
/// listed, so the CI total and the `user.slice` remainder are withheld.
#[cfg(unix)]
#[test]
fn the_runners_own_agent_builds_slice_is_found_where_user_slice_cannot_be_listed() {
    let (u, b) = (unit(), builds());
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[("", 0), ("user.slice", 0), (u.as_str(), 0), (b.as_str(), 0)]);
    let Some(_restore) = unlistable(&m.root.join("user.slice")) else {
        return;
    };
    let (s, _) = m.read(None, t0);
    m.usages(&[
        ("", 60_000_000),
        ("user.slice", 50_000_000),
        (u.as_str(), 5_000_000),
        (b.as_str(), 30_000_000),
    ]);
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    assert_eq!(m.placement().session_slices, vec![b.clone()]);
    assert_eq!(
        usec(&r, "session_usec"),
        Some(35_000_000),
        "the unit and the runner's own agent-builds.slice"
    );
    for key in ["ci_usec", "user_slice_remainder_usec", "unattributed_usec"] {
        assert!(
            r.attribution.get(key).is_none(),
            "{key}: a CI slice may be in user.slice"
        );
    }
    assert_eq!(r.attribution["withheld"], json!(["user_slice_unlisted"]));
}

/// A CI slice is looked for under every user manager, so while `user.slice`
/// cannot be listed one that was not found may be in a manager there: the CI
/// total, the `user.slice` remainder and `unattributed_usec` are withheld,
/// and session work, which is looked for by name, is not. Each layout leaves
/// out a different CI slice: `ci.slice` and its `ci-runners.slice` both, only
/// the watched `ci-runners.slice`, or only `ci.slice`. With both found outside
/// `user.slice`, nothing is withheld.
#[cfg(unix)]
#[test]
fn a_ci_slice_a_user_manager_that_could_not_be_listed_may_hold_is_withheld() {
    let u = unit();
    // (layout, ci.slice, ci-runners.slice, withheld)
    let layouts = [
        (
            "both in the runner's manager",
            format!("{MGR}/ci.slice"),
            format!("{MGR}/ci.slice/ci-runners.slice"),
            true,
        ),
        (
            "ci-runners.slice in the manager's ci.slice",
            "ci.slice".to_string(),
            format!("{MGR}/ci.slice/ci-runners.slice"),
            true,
        ),
        (
            "ci.slice in the manager",
            format!("{MGR}/ci.slice"),
            "ci-runners.slice".to_string(),
            true,
        ),
        (
            "both at the root",
            "ci.slice".to_string(),
            "ci.slice/ci-runners.slice".to_string(),
            false,
        ),
    ];
    for (layout, ci, runners, withheld) in layouts {
        let m = Machine::new(Some(&u));
        let t0 = Instant::now();
        m.usages(&[
            ("", 0),
            ("user.slice", 0),
            (u.as_str(), 0),
            (ci.as_str(), 0),
            (runners.as_str(), 0),
        ]);
        let Some(_restore) = unlistable(&m.root.join("user.slice")) else {
            return;
        };
        let (s, _) = m.read(None, t0);
        m.usages(&[
            ("", 100_000_000),
            ("user.slice", 50_000_000),
            (u.as_str(), 5_000_000),
            (ci.as_str(), 8_000_000),
            (runners.as_str(), 6_000_000),
        ]);
        let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
        assert_eq!(
            usec(&r, "session_usec"),
            Some(5_000_000),
            "{layout}: session work is found by name"
        );
        if withheld {
            for key in ["ci_usec", "user_slice_remainder_usec", "unattributed_usec"] {
                assert!(r.attribution.get(key).is_none(), "{layout}: {key}");
            }
            assert_eq!(
                r.attribution["withheld"],
                json!(["user_slice_unlisted"]),
                "{layout}"
            );
        } else {
            assert!(r.attribution.get("withheld").is_none(), "{layout}");
            assert_eq!(usec(&r, "ci_usec"), Some(8_000_000), "{layout}: ci.slice");
            assert_eq!(
                usec(&r, "user_slice_remainder_usec"),
                Some(45_000_000),
                "{layout}"
            );
            assert_eq!(usec(&r, "unattributed_usec"), Some(87_000_000), "{layout}");
        }
    }
}

/// A CI slice the previous read named in a user manager, while this read
/// could not list `user.slice`, is withheld as unlisted, not reported gone: it
/// may still be there, as it is here.
#[cfg(unix)]
#[test]
fn a_ci_slice_in_a_user_manager_that_could_not_be_listed_is_unlisted_not_gone() {
    let u = unit();
    let (ci, runners) = (
        format!("{MGR}/ci.slice"),
        format!("{MGR}/ci.slice/ci-runners.slice"),
    );
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        (u.as_str(), 0),
        (ci.as_str(), 0),
        (runners.as_str(), 0),
    ]);
    let (s, _) = m.read(None, t0);
    m.usages(&[
        ("", 100_000_000),
        ("user.slice", 50_000_000),
        (u.as_str(), 5_000_000),
        (ci.as_str(), 8_000_000),
        (runners.as_str(), 6_000_000),
    ]);
    let Some(_restore) = unlistable(&m.root.join("user.slice")) else {
        return;
    };
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    for key in ["ci_usec", "user_slice_remainder_usec", "unattributed_usec"] {
        assert!(r.attribution.get(key).is_none(), "{key}");
    }
    assert_eq!(
        r.attribution["withheld"],
        json!(["user_slice_unlisted"]),
        "the slice is still there"
    );
    assert_eq!(usec(&r, "session_usec"), Some(5_000_000));
}

/// A `user.slice` that does not exist (a runner that is a system service, on
/// a machine with no user manager) holds no user manager to look in: no CI
/// slice was found, and nothing is withheld for it.
#[test]
fn a_user_slice_that_does_not_exist_hides_no_ci_slice() {
    let u = "system.slice/qontinui-runner.service";
    let m = Machine::new(Some(u));
    let r = m.two_reads(
        &[("", 0), ("system.slice", 0), (u, 0)],
        &[
            ("", 40_000_000),
            ("system.slice", 30_000_000),
            (u, 10_000_000),
        ],
    );
    assert!(r.attribution.get("withheld").is_none());
    assert_eq!(usec(&r, "session_usec"), Some(10_000_000));
    assert_eq!(usec(&r, "system_slice_remainder_usec"), Some(20_000_000));
    assert_eq!(usec(&r, "unattributed_usec"), Some(30_000_000));
}

/// A dispatch cgroup is not a slice: one gone from the runner's unit while
/// `user.slice` cannot be listed is gone, and its time, now the unit's,
/// withholds session work.
#[cfg(unix)]
#[test]
fn a_dispatch_cgroup_gone_while_user_slice_cannot_be_listed_is_still_gone() {
    use crate::ci_node::cgroup::dispatch_dir_name;
    let u = unit();
    let job = format!("{u}/{}", dispatch_dir_name("job-1"));
    let m = Machine::new(Some(&u));
    let t0 = Instant::now();
    m.usages(&[
        ("", 0),
        ("user.slice", 0),
        (u.as_str(), 0),
        (job.as_str(), 0),
    ]);
    let Some(_restore) = unlistable(&m.root.join("user.slice")) else {
        return;
    };
    let (s, _) = m.read(None, t0);
    fs::remove_dir_all(m.root.join(&job)).unwrap();
    m.usages(&[
        ("", 100_000_000),
        ("user.slice", 50_000_000),
        (u.as_str(), 30_000_000),
    ]);
    let r = m.read(Some(&s), t0 + Duration::from_secs(30)).1.unwrap();
    assert!(
        r.attribution.get("session_usec").is_none(),
        "the unit holds the gone dispatch's time"
    );
    assert_eq!(
        r.attribution["withheld"],
        json!(["cgroup_gone", "user_slice_unlisted"])
    );
}

/// Session work is the runner's unit and the holder scopes together: a read
/// that finds no unit for the runner (it runs in the root cgroup, or its
/// `/proc/self/cgroup` could not be read) publishes no session total, since
/// the runner's own time would be missing from it.
#[test]
fn a_read_that_finds_no_unit_for_the_runner_publishes_no_session_total() {
    let a = holder("a");
    for own_cgroup in [Some("0::/\n"), None] {
        let m = Machine::new(None);
        if let Some(body) = own_cgroup {
            write(&m.procfs, "self/cgroup", body);
        }
        let r = m.two_reads(
            &[("", 0), ("user.slice", 0), (a.as_str(), 0)],
            &[
                ("", 50_000_000),
                ("user.slice", 30_000_000),
                (a.as_str(), 20_000_000),
            ],
        );
        for key in ["session_usec", "runner_unit_usec", "runner_unit"] {
            assert!(
                r.attribution.get(key).is_none(),
                "{key}: no runner unit ({own_cgroup:?})"
            );
        }
        assert_eq!(
            r.attribution["pty_holder_scopes_seen"],
            json!(1),
            "the holders of the runner's user are still found"
        );
        assert_eq!(usec(&r, "pty_holder_scopes_usec"), Some(20_000_000));
        // In the root cgroup the runner has no dispatch cgroup beside it. With
        // its own cgroup unknown, one could be anywhere.
        let (remainder, unattributed) = match own_cgroup {
            Some(_) => (Some(10_000_000), Some(30_000_000)),
            None => (None, None),
        };
        assert_eq!(
            usec(&r, "user_slice_remainder_usec"),
            remainder,
            "{own_cgroup:?}"
        );
        assert_eq!(
            usec(&r, "unattributed_usec"),
            unattributed,
            "{own_cgroup:?}"
        );
    }
}

#[test]
fn every_watched_slice_is_charged_to_a_class() {
    for slice in WATCHED_SLICES {
        assert!(watched_class(slice).is_some(), "{slice} has no class");
    }
}

#[test]
fn apply_adds_cpu_beside_memory_and_never_makes_an_entry() {
    let report = CpuReport {
        interval_usec: 30_000_000,
        slices: BTreeMap::from([("ci-runners.slice", 7), ("agent-builds.slice", 9)]),
        attribution: Map::new(),
    };
    let mut slices = json!({"ci-runners.slice": {"memory_current": 1}})
        .as_object()
        .unwrap()
        .clone();
    apply(&report, &mut slices);
    assert_eq!(
        Value::Object(slices),
        json!({"ci-runners.slice": {
            "memory_current": 1,
            "cpu": {"usage_delta_usec": 7, "interval_usec": 30_000_000}
        }}),
        "a slice with no memory entry gets none, and an empty attribution adds no key"
    );
}

/// coord#3567 (open; caps as of b461649cb) stores `cgroup_slices` only inside
/// its caps (16 top-level keys, 16 KiB, depth 8, no NUL), and the whole column
/// as NULL otherwise, memory readings included. This also pins the
/// attribution's key set.
#[test]
fn the_published_map_stays_inside_coords_caps_on_a_crowded_machine() {
    let u = unit();
    let m = Machine::new(Some(&u));
    let watched = [
        "ci.slice/ci-runners.slice".to_string(),
        builds(),
        format!("{MGR}/qontinui.slice/qontinui-runner.slice"),
    ];
    for w in &watched {
        write(&m.root, &format!("{w}/memory.current"), "1048576\n");
        write(&m.root, &format!("{w}/memory.high"), "max\n");
        write(&m.root, &format!("{w}/memory.max"), "max\n");
        write(
            &m.root,
            &format!("{w}/memory.events"),
            "low 0\nhigh 3\nmax 0\noom 0\noom_kill 0\n",
        );
    }
    let mut cgroups: Vec<(String, u64)> = vec![
        (String::new(), 10_000_000_000_000),
        ("user.slice".into(), 1_000_000_000_000),
        ("system.slice".into(), 1_000_000_000_000),
        ("ci.slice".into(), 100_000_000_000),
        (u.clone(), 1_000_000_000),
    ];
    cgroups.extend(watched.iter().map(|w| (w.clone(), 1_000_000_000)));
    cgroups.extend((0..300).map(|i| (holder(&format!("p{i:03}")), 1_000_000_000)));
    cgroups.extend((0..20).map(|i| {
        (
            format!("ci.slice/ci-pool-{i:02}-runners-for-a-long-named-tenant.slice"),
            1_000_000_000,
        )
    }));
    let at =
        |k: u64| -> Vec<(&str, u64)> { cgroups.iter().map(|(c, v)| (c.as_str(), v * k)).collect() };
    let r = m.two_reads(&at(1), &at(2));

    let mut slices = super::super::read_slices(&m.root).memory;
    apply(&r, &mut slices);
    let doc = Value::Object(slices);
    let bytes = serde_json::to_vec(&doc).unwrap();
    assert!(doc.as_object().unwrap().len() <= 16);
    assert!(bytes.len() <= 16 * 1024, "{} bytes", bytes.len());
    assert!(json_depth(&doc) <= 8);
    assert!(!has_nul(&doc));
    // The most a read that withholds can add is `withheld` with every reason
    // in `Why::ALL`.
    let withheld = json!({"withheld": Why::ALL.map(Why::key)});
    let most = bytes.len() + serde_json::to_vec(&withheld).unwrap().len();
    assert!(most <= 16 * 1024, "{most} bytes with every reason");
    for w in [
        "ci-runners.slice",
        "agent-builds.slice",
        "qontinui-runner.slice",
    ] {
        assert_eq!(
            doc[w]["cpu"]["usage_delta_usec"],
            json!(1_000_000_000),
            "{w}"
        );
        assert_eq!(
            doc[w]["memory_current"],
            json!(1_048_576),
            "{w}: memory kept"
        );
    }
    let a = &doc[ATTRIBUTION_KEY];
    assert_eq!(
        a["ci_children_usec"].as_object().unwrap().len(),
        MAX_CI_CHILDREN
    );
    assert_eq!(a["ci_children_omitted"], json!(21 - MAX_CI_CHILDREN));
    assert_eq!(a["pty_holder_scopes_seen"], json!(300));
    let keys: BTreeSet<&str> = a.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "interval_usec",
            "host_usec",
            "session_usec",
            "ci_usec",
            "unattributed_usec",
            "user_slice_remainder_usec",
            "system_slice_remainder_usec",
            "runner_unit",
            "runner_unit_usec",
            "pty_holder_scopes_usec",
            "pty_holder_scopes_measured",
            "pty_holder_scopes_seen",
            "pty_holder_scopes_ended",
            "ci_children_usec",
            "ci_children_unmeasured",
            "ci_children_omitted",
        ])
    );
}

/// `Why::ALL`, which the caps test above publishes whole, lists each reason
/// once and in the order `withheld` publishes them (their `Ord`), under keys
/// that differ.
#[test]
fn every_withheld_reason_is_listed_once_in_the_order_it_is_published() {
    assert!(
        Why::ALL.windows(2).all(|pair| pair[0] < pair[1]),
        "{:?}",
        Why::ALL
    );
    let keys: BTreeSet<&str> = Why::ALL.iter().map(|w| w.key()).collect();
    assert_eq!(keys.len(), Why::ALL.len(), "{keys:?}");
}
