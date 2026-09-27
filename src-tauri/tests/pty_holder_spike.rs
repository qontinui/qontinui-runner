//! Phase 0 go/no-go for plan
//! `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`:
//! does a PTY held by `qontinui-runner --pty-holder-spike` outlive the process
//! that spawned the holder — and is the surviving child the SAME process, not
//! a recycled pid?
//!
//! Every test name starts `pty_holder_spike_` so the plan's gate,
//! `cargo-guard.sh test pty_holder_spike`, selects exactly these (plus the
//! argv unit tests in `src/pty_holder/spike.rs`).
//!
//! Only processes these tests spawned are ever signalled, and a recorded pid
//! is signalled only while its start-time identity still matches, so cleanup
//! cannot hit a recycled pid.
//!
//! Windows and macOS arms need those boxes. The Windows tests below are
//! `#[ignore]`d and are recorded UNRUN on merytshost, never passed.

use std::io::{BufRead, BufReader};
use std::process::ChildStdout;
use std::sync::mpsc;
use std::time::Duration;

use qontinui_runner_lib::pty_holder::spike::{report_field, SPIKE_FLAG, SPIKE_PARENT_FLAG};

const RUNNER_BIN: &str = env!("CARGO_BIN_EXE_qontinui-runner");

/// Generous: a debug runner binary is large, and this box is often loaded.
const LINE_TIMEOUT: Duration = Duration::from_secs(120);

/// Read ONE line from `stdout` on a helper thread, bounded by [`LINE_TIMEOUT`].
fn read_line_bounded(stdout: ChildStdout) -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(LINE_TIMEOUT)
        .expect("no report line from the holder within the timeout")
        .trim_end()
        .to_string()
}

/// `(holder_pid, child_pid)` from a report line, panicking with the line.
fn pids_of(line: &str) -> (u32, u32) {
    match (
        report_field(line, "holder_pid"),
        report_field(line, "child_pid"),
    ) {
        (Some(h), Some(c)) => (h, c),
        _ => panic!("not a holder report line: {line:?}"),
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    /// What identifies a process across time: its pid plus its start time.
    /// Linux reads `/proc/<pid>/stat` field 22; other Unixes ask `ps`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Identity {
        pid: i32,
        start: String,
    }

    /// Fields of `/proc/<pid>/stat` we use (Linux).
    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct Stat {
        state: char,
        ppid: i32,
        session: i32,
        starttime: String,
    }

    #[cfg(target_os = "linux")]
    fn stat(pid: i32) -> Option<Stat> {
        let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm may contain spaces/parens: parse after the LAST ')'.
        let rest = &raw[raw.rfind(')')? + 1..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        // f[0] = field 3 (state) … f[19] = field 22 (starttime).
        Some(Stat {
            state: f.first()?.chars().next()?,
            ppid: f.get(1)?.parse().ok()?,
            session: f.get(3)?.parse().ok()?,
            starttime: f.get(19)?.to_string(),
        })
    }

    #[cfg(target_os = "linux")]
    fn start_of(pid: i32) -> Option<String> {
        stat(pid).map(|s| s.starttime)
    }

    #[cfg(not(target_os = "linux"))]
    fn start_of(pid: i32) -> Option<String> {
        let out = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn identity(pid: u32) -> Identity {
        let pid = pid as i32;
        Identity {
            pid,
            start: start_of(pid).unwrap_or_else(|| panic!("pid {pid} has no start time")),
        }
    }

    /// Alive = signalable AND not a zombie (a zombie answers `kill(pid, 0)`).
    fn is_alive(pid: i32) -> bool {
        // SAFETY: signal 0 performs only the existence/permission check.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            stat(pid).is_some_and(|s| s.state != 'Z')
        }
        #[cfg(not(target_os = "linux"))]
        {
            Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .map(|o| {
                    !String::from_utf8_lossy(&o.stdout)
                        .trim_start()
                        .starts_with('Z')
                })
                .unwrap_or(false)
        }
    }

    /// The same process is still running: alive, and its start time is the
    /// one recorded before the kill (rules out pid reuse).
    fn still_same(id: &Identity) -> bool {
        is_alive(id.pid) && start_of(id.pid).as_deref() == Some(id.start.as_str())
    }

    /// Kills everything this test spawned on drop — including on a failed
    /// assertion — and only while each recorded identity still matches.
    #[derive(Default)]
    struct Reap {
        ids: Vec<Identity>,
        children: Vec<Child>,
    }

    impl Drop for Reap {
        fn drop(&mut self) {
            for id in &self.ids {
                if start_of(id.pid).as_deref() == Some(id.start.as_str()) {
                    // SAFETY: plain signal to a pid we spawned and re-verified.
                    unsafe {
                        libc::kill(id.pid, libc::SIGKILL);
                    }
                }
            }
            for c in &mut self.children {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    fn wait_until(deadline: Duration, mut f: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        f()
    }

    /// The go/no-go, spelled the way the incident happens: the spawner's
    /// WHOLE process group is SIGKILLed (a closed terminal, a killed service
    /// cgroup's leader). The parent is a plain `sh` that launches the holder in
    /// the background, so nothing but the holder's own `setsid()` separates it.
    #[test]
    fn pty_holder_spike_child_survives_sigkill_of_spawner_process_group() {
        let mut reap = Reap::default();
        let mut parent = Command::new("sh")
            .arg("-c")
            .arg(r#""$0" "$1" -- sleep 3600 & wait"#)
            .arg(RUNNER_BIN)
            .arg(SPIKE_FLAG)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sh parent");
        let parent_pid = parent.id() as i32;
        let line = read_line_bounded(parent.stdout.take().unwrap());
        reap.children.push(parent);
        let (holder_pid, child_pid) = pids_of(&line);
        let holder = identity(holder_pid);
        let child = identity(child_pid);
        reap.ids.push(child.clone());
        reap.ids.push(holder.clone());

        #[cfg(target_os = "linux")]
        {
            let hs = stat(holder.pid).expect("holder stat");
            let ps = stat(parent_pid).expect("parent stat");
            assert_eq!(
                hs.session, holder.pid,
                "holder must lead its own session (setsid)"
            );
            assert_ne!(
                hs.session, ps.session,
                "holder must not share the spawner's session"
            );
            let cs = stat(child.pid).expect("child stat");
            assert_eq!(
                cs.ppid, holder.pid,
                "the PTY child must be the holder's child"
            );
        }

        // SIGKILL the spawner's whole process group (pgid == parent pid).
        // SAFETY: a negative pid addresses the group this test created.
        assert_eq!(unsafe { libc::kill(-parent_pid, libc::SIGKILL) }, 0);
        let p = reap.children.last_mut().unwrap();
        let _ = p.wait();
        assert!(!is_alive(parent_pid), "parent should be dead");

        // Give any delayed teardown (SIGHUP, reaper) time to land.
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            still_same(&child),
            "GO/NO-GO FAILED: PTY child {child:?} did not survive SIGKILL of its spawner's group"
        );
        assert!(
            still_same(&holder),
            "holder {holder:?} died with its spawner"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            stat(child.pid).unwrap().ppid,
            holder.pid,
            "the surviving child must still be parented by the holder"
        );
    }

    /// Same question with the runner's own spawner stand-in
    /// (`--pty-holder-spike-parent`), i.e. the spawn path Phase 1 will grow,
    /// SIGKILLed alone.
    #[test]
    fn pty_holder_spike_child_survives_sigkill_of_runner_parent() {
        use std::io::Write;
        let mut reap = Reap::default();
        let mut parent = Command::new(RUNNER_BIN)
            .args([SPIKE_PARENT_FLAG, "--", "sleep", "3600"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn runner parent");
        let parent_pid = parent.id() as i32;
        parent
            .stdin
            .take()
            .unwrap()
            .write_all(b"go\n")
            .expect("write go line");
        let line = read_line_bounded(parent.stdout.take().unwrap());
        reap.children.push(parent);
        assert!(
            line.ends_with("route=auto"),
            "unexpected relay line {line:?}"
        );
        let (holder_pid, child_pid) = pids_of(&line);
        let holder = identity(holder_pid);
        let child = identity(child_pid);
        reap.ids.push(child.clone());
        reap.ids.push(holder.clone());

        // SAFETY: SIGKILL to the parent this test spawned.
        assert_eq!(unsafe { libc::kill(parent_pid, libc::SIGKILL) }, 0);
        let _ = reap.children.last_mut().unwrap().wait();

        std::thread::sleep(Duration::from_secs(1));
        assert!(
            still_same(&child),
            "GO/NO-GO FAILED: PTY child {child:?} did not survive SIGKILL of the runner parent"
        );
        assert!(
            still_same(&holder),
            "holder {holder:?} died with its parent"
        );
    }

    /// A holder that is spawned as a process-GROUP LEADER cannot `setsid()`
    /// (EPERM). It must still detach — by re-exec'ing itself — and survive a
    /// SIGKILL of the group it was born in. Found on merytshost: a bash with
    /// job control puts every `cmd &` in its own group.
    #[test]
    fn pty_holder_spike_group_leader_holder_still_detaches() {
        let mut reap = Reap::default();
        let mut spawned = Command::new(RUNNER_BIN)
            .args([SPIKE_FLAG, "--", "sleep", "3600"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn group-leader holder");
        let group = spawned.id() as i32;
        let line = read_line_bounded(spawned.stdout.take().unwrap());
        reap.children.push(spawned);
        let (holder_pid, child_pid) = pids_of(&line);
        assert_ne!(
            holder_pid as i32, group,
            "a group leader must hand off to a re-exec'd, setsid()'d holder"
        );
        let holder = identity(holder_pid);
        let child = identity(child_pid);
        reap.ids.push(child.clone());
        reap.ids.push(holder.clone());

        // The intermediate may already have exited (ESRCH is fine).
        // SAFETY: a negative pid addresses the group this test created.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = reap.children.last_mut().unwrap().wait();
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            still_same(&child),
            "PTY child {child:?} died with the holder's birth group"
        );
        assert!(
            still_same(&holder),
            "holder {holder:?} died with its birth group"
        );
    }

    /// The coupling that must REMAIN: a pane whose holder dies ends (the
    /// master closes, the child's terminal hangs up). Without this the holder
    /// would be a leak factory rather than an owner.
    #[test]
    fn pty_holder_spike_child_ends_when_its_holder_dies() {
        let mut reap = Reap::default();
        let mut holder_proc = Command::new(RUNNER_BIN)
            .args([SPIKE_FLAG, "--", "sleep", "3600"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn holder");
        let line = read_line_bounded(holder_proc.stdout.take().unwrap());
        reap.children.push(holder_proc);
        let (holder_pid, child_pid) = pids_of(&line);
        let child = identity(child_pid);
        reap.ids.push(child.clone());
        assert_eq!(holder_pid, reap.children[0].id());

        let _ = reap.children[0].kill(); // SIGKILL the holder
        let _ = reap.children[0].wait();
        assert!(
            wait_until(Duration::from_secs(10), || !still_same(&child)),
            "PTY child {child:?} outlived its holder — the master close did not hang it up"
        );
    }

    /// Phase 0 measurement (plan Risk 6 / D13): per-holder memory and threads
    /// of the re-exec'd runner binary in holder mode, at 1 and at 40 holders.
    /// Ignored: it is a measurement, not an assertion. Run with
    /// `cargo-guard.sh test pty_holder_spike -- --ignored --nocapture`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "measurement, run by hand: prints per-holder RSS/PSS/threads at 1 and 40 holders"]
    fn pty_holder_spike_measure_holder_footprint() {
        fn status_kb(pid: u32, key: &str) -> u64 {
            let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            s.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        }
        fn pss_kb(pid: u32) -> u64 {
            let s =
                std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap_or_default();
            s.lines()
                .find_map(|l| l.strip_prefix("Pss:"))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
                .unwrap_or(0)
        }
        fn report(label: &str, pids: &[u32]) {
            let rows: Vec<[u64; 5]> = pids
                .iter()
                .map(|&p| {
                    [
                        status_kb(p, "VmRSS"),
                        status_kb(p, "RssAnon"),
                        status_kb(p, "RssFile"),
                        pss_kb(p),
                        status_kb(p, "Threads"),
                    ]
                })
                .collect();
            let n = rows.len() as u64;
            let sum = |i: usize| rows.iter().map(|r| r[i]).sum::<u64>();
            let max = |i: usize| rows.iter().map(|r| r[i]).max().unwrap_or(0);
            println!(
                "[{label}] holders={n} | per-holder avg: VmRSS={} kB RssAnon={} kB RssFile={} kB Pss={} kB Threads={} | max VmRSS={} kB max Threads={} | total Pss={} kB",
                sum(0) / n, sum(1) / n, sum(2) / n, sum(3) / n, sum(4) as f64 / n as f64,
                max(0), max(4), sum(3)
            );
        }

        let bin_size = std::fs::metadata(RUNNER_BIN).map(|m| m.len()).unwrap_or(0);
        println!("runner binary: {RUNNER_BIN} ({} MB)", bin_size / 1_000_000);

        let mut reap = Reap::default();
        let mut holders: Vec<u32> = Vec::new();
        let mut spawn_ms: Vec<u128> = Vec::new();
        let mut spawn_one = |reap: &mut Reap| {
            let t = std::time::Instant::now();
            let mut h = Command::new(RUNNER_BIN)
                .args([SPIKE_FLAG, "--", "sleep", "3600"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn holder");
            let line = read_line_bounded(h.stdout.take().unwrap());
            spawn_ms.push(t.elapsed().as_millis());
            let (hp, cp) = pids_of(&line);
            reap.ids.push(identity(cp));
            reap.children.push(h);
            hp
        };

        holders.push(spawn_one(&mut reap));
        std::thread::sleep(Duration::from_millis(500));
        report("1", &holders);
        while holders.len() < 40 {
            holders.push(spawn_one(&mut reap));
        }
        std::thread::sleep(Duration::from_millis(500));
        report("40", &holders);
        let mut sorted = spawn_ms.clone();
        sorted.sort_unstable();
        println!(
            "spawn-to-report latency ms: first={} median={} max={}",
            spawn_ms[0],
            sorted[sorted.len() / 2],
            sorted[sorted.len() - 1]
        );
    }
}

/// Windows arms (plan D4 as vetted 2026-09-27). The killing case is the
/// PARENT inside an OUTER `KILL_ON_JOB_CLOSE` job — the supervisor's shape —
/// TerminateProcess'd, and then that job's last handle closed.
#[cfg(windows)]
mod windows {
    use super::*;
    use qontinui_runner_win32::holder_spawn::OuterKillOnCloseJob;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, TerminateProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    const STILL_ACTIVE: u32 = 259;

    /// `(alive, creation time)` for `pid`; `None` when it cannot be opened.
    fn probe(pid: u32) -> Option<(bool, u64)> {
        // SAFETY: handle opened, queried and closed within this block.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut code = 0u32;
            let alive = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE;
            let z = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut c, mut e, mut k, mut u) = (z, z, z, z);
            GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
            CloseHandle(h);
            Some((
                alive,
                ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64,
            ))
        }
    }

    fn still_same(pid: u32, created: u64) -> bool {
        matches!(probe(pid), Some((true, c)) if c == created)
    }

    struct Reap(Vec<(u32, u64)>);
    impl Drop for Reap {
        fn drop(&mut self) {
            for &(pid, created) in &self.0 {
                if still_same(pid, created) {
                    // SAFETY: terminating a process this test spawned and re-verified.
                    unsafe {
                        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                        if !h.is_null() {
                            TerminateProcess(h, 1);
                            CloseHandle(h);
                        }
                    }
                }
            }
        }
    }

    /// Spawn the runner parent (optionally inside an outer job), have it spawn
    /// a holder by `route`, kill the parent, close the job, and report whether
    /// the PTY child survived as the same process.
    fn child_survives(outer_job: Option<bool>, route: &str) -> bool {
        let job = outer_job.map(|bok| OuterKillOnCloseJob::create(bok).expect("create job"));
        let mut parent = Command::new(RUNNER_BIN)
            .args([SPIKE_PARENT_FLAG, "--route", route])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn runner parent");
        if let Some(j) = &job {
            j.assign(&parent).expect("assign parent to outer job");
        }
        parent
            .stdin
            .take()
            .unwrap()
            .write_all(b"go\n")
            .expect("write go line");
        let line = read_line_bounded(parent.stdout.take().unwrap());
        let (holder_pid, child_pid) = pids_of(&line);
        let (_, holder_created) = probe(holder_pid).expect("probe holder");
        let (_, child_created) = probe(child_pid).expect("probe child");
        let _reap = Reap(vec![
            (child_pid, child_created),
            (holder_pid, holder_created),
        ]);

        let _ = parent.kill(); // TerminateProcess
        let _ = parent.wait();
        drop(job); // last handle: KILL_ON_JOB_CLOSE fires on whatever is still in it
        std::thread::sleep(Duration::from_secs(2));
        still_same(child_pid, child_created)
    }

    #[test]
    #[ignore = "needs a Windows box; recorded UNRUN on merytshost"]
    fn pty_holder_spike_windows_no_breakaway_in_outer_job_dies() {
        assert!(!child_survives(Some(true), "plain"));
    }

    #[test]
    #[ignore = "needs a Windows box; recorded UNRUN on merytshost"]
    fn pty_holder_spike_windows_breakaway_with_breakaway_ok_survives() {
        assert!(child_survives(Some(true), "breakaway"));
    }

    #[test]
    #[ignore = "needs a Windows box AND the WMI fallback, which Phase 0 leaves as \
                HolderSpawnError::NeedsWmiFallback; recorded UNRUN on merytshost"]
    fn pty_holder_spike_windows_wmi_survives_job_without_breakaway_ok() {
        assert!(child_survives(Some(false), "wmi"));
    }

    #[test]
    #[ignore = "needs a Windows box; recorded UNRUN on merytshost"]
    fn pty_holder_spike_windows_no_job_control_survives() {
        assert!(child_survives(None, "auto"));
    }
}
