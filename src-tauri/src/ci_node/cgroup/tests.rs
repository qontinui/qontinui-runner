#![cfg(test)]

use super::*;

/// A fixture parent that behaves like a delegated cgroup for the file
/// reads/writes `create_in` does (plain files stand in for cgroupfs).
fn fixture_parent(controllers: &str, subtree: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("cgroup.controllers"), controllers).unwrap();
    fs::write(d.path().join("cgroup.subtree_control"), subtree).unwrap();
    d
}

#[test]
fn dir_name_is_prefixed_and_path_safe() {
    assert!(dispatch_dir_name("abc-123").starts_with("dispatch-abc-123-"));
    assert!(dispatch_dir_name("../../etc").starts_with("dispatch-______etc-"));
    assert!(dispatch_dir_name("").starts_with("dispatch-x-"));
    assert_eq!(
        dispatch_dir_name(&"a".repeat(200)).len(),
        "dispatch-".len() + 64 + 9
    );
    assert!(!dispatch_dir_name("a/b").contains('/'));
    assert!(!dispatch_dir_name("..").contains(".."));
    // Ids that sanitize alike still get distinct directories.
    assert_ne!(dispatch_dir_name("a/b"), dispatch_dir_name("a?b"));
    assert_ne!(dispatch_dir_name("\u{e9}"), dispatch_dir_name("\u{fc}"));
}

#[test]
fn unified_cgroup_is_parsed_from_proc() {
    assert_eq!(
        parse_unified_cgroup("0::/system.slice/qontinui-runner.service/runner\n").as_deref(),
        Some("/system.slice/qontinui-runner.service/runner")
    );
    assert_eq!(
        parse_unified_cgroup("0::/a/b (deleted)\n").as_deref(),
        Some("/a/b")
    );
    // cgroup v1 only: no `0::` line, so no containment.
    assert_eq!(parse_unified_cgroup("12:memory:/foo\n11:cpu:/foo\n"), None);
}

#[test]
fn service_parent_is_a_candidate_only_for_service_units() {
    assert_eq!(
        candidate_parents("/system.slice/qontinui-runner.service/runner"),
        vec![
            "/system.slice/qontinui-runner.service/runner".to_string(),
            "/system.slice/qontinui-runner.service".to_string()
        ]
    );
    // Never reach up into a shared slice.
    assert_eq!(
        candidate_parents("/system.slice/qontinui-runner.service"),
        vec!["/system.slice/qontinui-runner.service".to_string()]
    );
    assert_eq!(candidate_parents("/"), vec!["/".to_string()]);
    assert_eq!(candidate_parents(""), vec!["/".to_string()]);
}

#[test]
fn cgroup_dir_joins_under_the_mount() {
    assert_eq!(
        cgroup_dir(Path::new("/sys/fs/cgroup"), "/a/b"),
        PathBuf::from("/sys/fs/cgroup/a/b")
    );
    assert_eq!(
        cgroup_dir(Path::new("/sys/fs/cgroup"), "/"),
        PathBuf::from("/sys/fs/cgroup/")
    );
}

#[test]
fn controller_and_event_parsers() {
    assert!(lists_memory("cpu io memory pids\n"));
    assert!(!lists_memory("cpu io pids\n"));
    assert!(!lists_memory("memory_recursiveprot\n"));
    assert_eq!(parse_procs("12\n34\nx\n\n"), vec![12, 34]);
    assert_eq!(
        parse_oom_kill("low 0\nhigh 0\nmax 3\noom 2\noom_kill 2\noom_group_kill 0\n"),
        Some(2)
    );
    assert_eq!(parse_oom_kill("low 0\n"), None);
}

#[test]
fn create_writes_the_limit_the_windows_arm_uses() {
    let p = fixture_parent("cpu memory pids", "memory");
    let limit = super::super::executor::CI_JOB_MEMORY_LIMIT_BYTES;
    let c = CgroupDispatch::create_in(p.path(), "d1", limit).unwrap();
    assert_eq!(c.dir(), p.path().join(dispatch_dir_name("d1")));
    let written = fs::read_to_string(c.dir().join("memory.max")).unwrap();
    assert_eq!(written, (32u64 * 1024 * 1024 * 1024).to_string());
    assert_eq!(
        fs::read_to_string(c.dir().join("memory.oom.group")).unwrap(),
        "1"
    );
    // Fixture dirs hold plain files, so Drop's rmdir cannot succeed; the
    // TempDir owns the cleanup.
}

#[test]
fn missing_memory_controller_degrades_with_a_reason() {
    let p = fixture_parent("cpu pids", "");
    let e = CgroupDispatch::create_in(p.path(), "d2", 1 << 20).unwrap_err();
    assert!(e.contains("memory controller is not available"), "{e}");
    assert!(
        !p.path().join(dispatch_dir_name("d2")).exists(),
        "nothing left behind"
    );
}

#[test]
fn unreadable_parent_degrades_with_a_reason() {
    let d = tempfile::tempdir().unwrap();
    let e = CgroupDispatch::create_in(&d.path().join("nope"), "d3", 1 << 20).unwrap_err();
    assert!(e.contains("cgroup.controllers unreadable"), "{e}");
}

#[test]
fn memory_is_enabled_for_children_when_listed_but_not_enabled() {
    let p = fixture_parent("memory", "");
    let _c = CgroupDispatch::create_in(p.path(), "d4", 1 << 20).unwrap();
    assert_eq!(
        fs::read_to_string(p.path().join("cgroup.subtree_control")).unwrap(),
        "+memory"
    );
}

#[test]
fn kill_prefers_cgroup_kill_and_falls_back_to_the_procs_loop() {
    let p = fixture_parent("memory", "memory");
    let c = CgroupDispatch::create_in(p.path(), "d5", 1 << 20).unwrap();
    // Writable cgroup.kill: the atomic path.
    assert_eq!(c.kill(), KillPath::CgroupKill);
    assert_eq!(
        fs::read_to_string(c.dir().join("cgroup.kill")).unwrap(),
        "1"
    );
    // cgroup.kill not writable (kernel < 5.14): a directory there makes
    // the write fail; an empty cgroup.procs ends the loop at once.
    fs::remove_file(c.dir().join("cgroup.kill")).unwrap();
    fs::create_dir(c.dir().join("cgroup.kill")).unwrap();
    fs::write(c.dir().join("cgroup.procs"), "").unwrap();
    assert_eq!(c.kill(), KillPath::ProcsLoop);
}

#[test]
fn oom_note_reports_each_kill_once() {
    let p = fixture_parent("memory", "memory");
    let c = CgroupDispatch::create_in(p.path(), "d6", 4096).unwrap();
    assert_eq!(
        c.take_oom_note(),
        None,
        "no memory.events yet is not a kill"
    );
    fs::write(c.dir().join("memory.events"), "max 5\noom_kill 0\n").unwrap();
    assert_eq!(c.take_oom_note(), None);
    fs::write(c.dir().join("memory.events"), "max 9\noom_kill 1\n").unwrap();
    let note = c.take_oom_note().expect("a new oom_kill is reported");
    assert!(
        note.contains("memory limit") && note.contains("4096"),
        "{note}"
    );
    assert_eq!(c.take_oom_note(), None, "reported once");
}

#[test]
fn the_env_override_is_honoured_and_describe_names_the_state() {
    // Fixture root via the override. The shared env lock excludes every
    // other env-touching test, including the real-kernel cgroup test that
    // reads the variable.
    let _g = crate::test_env::env_lock();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            std::env::remove_var(CGROUP_ROOT_ENV);
        }
    }
    let p = fixture_parent("memory", "memory");
    std::env::set_var(CGROUP_ROOT_ENV, p.path());
    let _restore = Restore;
    let c = Containment::create("d7", 1 << 20);
    drop(_restore);
    assert!(matches!(c, Containment::Cgroup(_)));
    assert!(
        c.describe().contains("memory.max=1048576"),
        "{}",
        c.describe()
    );
    let u = Containment::Uncontained("no delegation".into());
    assert!(u.describe().contains("UNCONTAINED"));
    assert!(u.describe().contains("no delegation"));
}

/// Every dispatch is counted once its containment is settled, and an
/// uncontained one counts as begun, and as running until it is dropped.
/// The slice telemetry reads these to find a dispatch none of its reads
/// listed. Exact counts hold because every test that makes an
/// `UncontainedRun` holds the env lock for as long as the run lives, and a
/// new one must too: one that makes a `Containment`, or that adopts a child
/// into a cgroup refusing it, as the real-kernel test below does under the
/// lock, ending any stray run before it lets go. The executor's
/// `CiJob::create`, and the [`OutsideCgroup`] its `run_dispatch` holds,
/// are reached only through `admission::submit`, which no test calls.
#[test]
fn dispatches_are_counted_and_an_uncontained_one_while_it_runs() {
    let _g = crate::test_env::env_lock();
    let _restore = crate::test_env::EnvVarRestore::capture(&[CGROUP_ROOT_ENV]);
    let before = dispatch_counts();

    let bare = fixture_parent("cpu io", "");
    std::env::set_var(CGROUP_ROOT_ENV, bare.path());
    let u = Containment::create("d8", 1 << 20);
    assert!(
        matches!(u, Containment::Uncontained(_)),
        "no memory controller"
    );
    assert_eq!(
        dispatch_counts(),
        DispatchCounts {
            started: before.started + 1,
            uncontained_running: before.uncontained_running + 1,
            uncontained_begun: before.uncontained_begun + 1,
        }
    );
    drop(u);
    assert_eq!(
        dispatch_counts(),
        DispatchCounts {
            started: before.started + 1,
            uncontained_running: before.uncontained_running,
            uncontained_begun: before.uncontained_begun + 1,
        },
        "an ended dispatch no longer runs, and still began"
    );

    let delegated = fixture_parent("memory", "memory");
    std::env::set_var(CGROUP_ROOT_ENV, delegated.path());
    let c = Containment::create("d9", 1 << 20);
    assert!(matches!(c, Containment::Cgroup(_)));
    assert_eq!(
        dispatch_counts(),
        DispatchCounts {
            started: before.started + 2,
            uncontained_running: before.uncontained_running,
            uncontained_begun: before.uncontained_begun + 1,
        },
        "a contained dispatch is started, not uncontained"
    );
}

/// `ci_node`'s own work for a dispatch counts as an uncontained run while
/// the dispatch is set up and again once its steps are over, never as a
/// started dispatch, and one run at a time.
#[test]
fn ci_nodes_own_work_outside_the_dispatch_cgroup_counts_as_an_uncontained_run() {
    let _g = crate::test_env::env_lock();
    let before = dispatch_counts();
    let counts = |running: u64, begun: u64| DispatchCounts {
        started: before.started,
        uncontained_running: before.uncontained_running + running,
        uncontained_begun: before.uncontained_begun + begun,
    };

    let mut outside = OutsideCgroup::setup("d11");
    assert_eq!(dispatch_counts(), counts(1, 1), "the setup runs");
    outside.settled();
    assert_eq!(dispatch_counts(), counts(0, 1), "the containment counts");
    outside.after_steps();
    assert_eq!(dispatch_counts(), counts(1, 2), "the wind-down runs");
    outside.after_steps();
    assert_eq!(dispatch_counts(), counts(1, 2), "one run at a time");
    drop(outside);
    assert_eq!(dispatch_counts(), counts(0, 2), "the dispatch ended");

    let mut unsettled = OutsideCgroup::setup("d12");
    unsettled.after_steps();
    assert_eq!(
        dispatch_counts(),
        counts(1, 3),
        "a setup never settled goes on counting"
    );
    drop(unsettled);
    assert_eq!(dispatch_counts(), counts(0, 3), "and ends when it drops");
}

/// A contained dispatch whose step cannot be moved into its cgroup runs
/// that step in the runner's own cgroup, beside session work. It counts as
/// one uncontained run from that step until the dispatch drops, however
/// many of its steps fail to move.
#[cfg(unix)]
#[test]
fn a_dispatch_whose_steps_cannot_be_placed_runs_uncontained_until_it_drops() {
    let _g = crate::test_env::env_lock();
    let p = fixture_parent("memory", "memory");
    let c = CgroupDispatch::create_in(p.path(), "d10", 1 << 20).unwrap();
    // A directory where `cgroup.procs` would be refuses every write, as the
    // kernel refuses a move the writer may not make.
    fs::create_dir(c.dir().join("cgroup.procs")).unwrap();
    let before = dispatch_counts();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let step = || {
        rt.block_on(async {
            // console-ok: test-only file (#![cfg(test)]), unix cgroup fixture; never built into a release binary
            let mut cmd = tokio::process::Command::new("true");
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            c.prepare(&mut cmd);
            let mut child = cmd.spawn().expect("spawn true");
            c.adopt(&child);
            child.wait().await.expect("wait for true");
        })
    };
    step();
    let running = DispatchCounts {
        started: before.started,
        uncontained_running: before.uncontained_running + 1,
        uncontained_begun: before.uncontained_begun + 1,
    };
    assert_eq!(dispatch_counts(), running, "the first unplaced step");
    step();
    assert_eq!(dispatch_counts(), running, "counted once per dispatch");
    drop(c);
    assert_eq!(
        dispatch_counts(),
        DispatchCounts {
            uncontained_running: before.uncontained_running,
            ..running
        },
        "the dispatch ended"
    );
}

/// The other side of the test above: a step whose move into the dispatch
/// cgroup is taken makes no uncontained run, so a contained dispatch whose
/// steps are all placed never withholds the session / CI split.
#[cfg(unix)]
#[test]
fn a_dispatch_whose_steps_are_placed_makes_no_uncontained_run() {
    let _g = crate::test_env::env_lock();
    let p = fixture_parent("memory", "memory");
    let c = CgroupDispatch::create_in(p.path(), "d13", 1 << 20).unwrap();
    // A plain file where `cgroup.procs` would be takes every write, as the
    // kernel takes a move the writer may make.
    fs::write(c.dir().join("cgroup.procs"), "").unwrap();
    let before = dispatch_counts();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for _ in 0..2 {
        rt.block_on(async {
            // console-ok: test-only file (#![cfg(test)]), unix cgroup fixture; never built into a release binary
            let mut cmd = tokio::process::Command::new("true");
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            c.prepare(&mut cmd);
            let mut child = cmd.spawn().expect("spawn true");
            c.adopt(&child);
            child.wait().await.expect("wait for true");
        });
    }
    assert_eq!(dispatch_counts(), before, "placed steps run contained");
    drop(c);
    assert_eq!(dispatch_counts(), before, "and nothing ends with them");
}

/// Real-kernel check: a process exceeding `memory.max` is killed by the
/// kernel, reported as a non-success exit (the executor maps that to a
/// FAILURE), and does not hang. Skips with a message when the box offers
/// no delegated writable cgroup v2 subtree (CI containers, dev laptops).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_dispatch_exceeding_memory_max_is_killed_and_fails() {
    use std::os::unix::process::ExitStatusExt;
    let limit: u64 = 64 * 1024 * 1024;
    // The lock covers only the env read inside discovery (never an await).
    let cg = {
        let _g = crate::test_env::env_lock();
        match CgroupDispatch::discover_and_create(&format!("memtest-{}", std::process::id()), limit)
        {
            Ok(c) => c,
            Err(why) => {
                eprintln!("SKIPPED: no delegated writable cgroup v2 subtree here: {why}");
                return;
            }
        }
    };
    // Gated start: the hog (`tail -n 1 /dev/zero` buffers an endless
    // "line") only begins after placement is verified, so a placement
    // failure can never leave an unbounded hog running on the host.
    // console-ok: test-only file (#![cfg(test)]), unix cgroup fixture; never built into a release binary
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", "read x; exec tail -n 1 /dev/zero"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cg.prepare(&mut cmd);
    let mut child = cmd.spawn().expect("spawn sh");
    // Under the env lock: an adopt that could not place the child makes a
    // stray run, which another test's dispatch counts would see. On a
    // failed placement the run ends with `cg`, dropped at the end of this
    // block before the lock is (the guard is declared first).
    let placed = {
        let _g = crate::test_env::env_lock();
        let cg = cg;
        cg.adopt(&child);
        let pid = child.id().expect("pid");
        let placed = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        let want = cg.dir().file_name().unwrap().to_string_lossy().into_owned();
        if placed.contains(&want) {
            Ok(cg)
        } else {
            Err(format!("child not placed in {want}: {placed:?}"))
        }
    };
    let cg = match placed {
        Ok(cg) => cg,
        Err(why) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            panic!("{why}");
        }
    };
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"go\n").await.expect("release");
    }
    let status = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait())
        .await
        .expect("an over-limit dispatch must be killed, not hang")
        .expect("wait");
    assert!(
        !status.success(),
        "over-limit dispatch must not succeed: {status:?}"
    );
    assert_eq!(
        status.signal(),
        Some(9),
        "killed by SIGKILL (OOM): {status:?}"
    );
    assert!(
        cg.take_oom_note().is_some(),
        "memory.events should record the oom_kill"
    );
    // Drop kills strays and rmdirs; verify the directory is gone.
    let dir = cg.dir().to_path_buf();
    drop(cg);
    for _ in 0..80 {
        if !dir.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!dir.exists(), "cgroup directory removed after the dispatch");
}
