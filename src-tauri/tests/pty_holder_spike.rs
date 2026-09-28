//! Phase 0 go/no-go for plan
//! `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`:
//! does a PTY held by `qontinui-runner --pty-holder-spike` outlive the process
//! that spawned the holder — and is the surviving child the SAME process, not
//! a recycled pid?
//!
//! Every test name starts `pty_holder_spike_` so the plan's gate,
//! `cargo-guard.sh test pty_holder_spike`, selects exactly these (plus the
//! unit tests in `src/pty_holder/spike.rs`).
//!
//! Cleanup discipline: every spawned `Child`, every pid identity and every
//! transient systemd unit is registered with a drop guard the moment it
//! exists, so a timeout or failed assertion leaks nothing. Only processes and
//! units these tests created are ever signalled or stopped, and a recorded pid
//! is signalled only while its start-time identity still matches.
//!
//! Windows and macOS arms need those boxes. The Windows tests below are
//! `#[ignore]`d and are recorded UNRUN on merytshost, never passed.

use std::io::{BufRead, BufReader};
use std::process::ChildStdout;
use std::sync::mpsc;
use std::time::Duration;

use qontinui_runner_lib::pty_holder::spike::{
    report_field, report_str, SPIKE_FLAG, SPIKE_PARENT_FLAG,
};

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
    use std::path::PathBuf;
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

    /// Kills everything this test created on drop — including on a failed
    /// assertion or a timeout. Units first (stopping a unit kills its whole
    /// cgroup), then pids whose identity still matches, then `Child` handles,
    /// then files.
    #[derive(Default)]
    struct Reap {
        units: Vec<String>,
        ids: Vec<Identity>,
        children: Vec<Child>,
        files: Vec<PathBuf>,
    }

    impl Reap {
        /// Register `child` and hand back its stdout — registration happens
        /// BEFORE anything can block or panic.
        fn adopt(&mut self, mut child: Child) -> (usize, Option<ChildStdout>) {
            let out = child.stdout.take();
            self.children.push(child);
            (self.children.len() - 1, out)
        }

        fn track(&mut self, pid: u32) -> Identity {
            let id = identity(pid);
            self.ids.push(id.clone());
            id
        }
    }

    impl Drop for Reap {
        fn drop(&mut self) {
            for unit in &self.units {
                let _ = Command::new("systemctl")
                    .args(["--user", "stop", "--quiet", unit])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
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
            for f in &self.files {
                let _ = std::fs::remove_file(f);
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
        let parent = Command::new("sh")
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
        let (pi, out) = reap.adopt(parent);
        let line = read_line_bounded(out.unwrap());
        let (holder_pid, child_pid) = pids_of(&line);
        let child = reap.track(child_pid);
        let holder = reap.track(holder_pid);

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
        let _ = reap.children[pi].wait();
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
    /// (`--pty-holder-spike-parent`, `auto` route), i.e. the spawn path Phase 1
    /// will grow, SIGKILLed alone. `auto` resolves to `scope` wherever this
    /// test itself runs inside a systemd unit, so the holder's unit is
    /// registered for cleanup when one is reported.
    #[test]
    fn pty_holder_spike_child_survives_sigkill_of_runner_parent() {
        use qontinui_runner_lib::pty_holder::spike::scope_route_applies;
        use std::io::Write;
        let mut reap = Reap::default();
        let mut parent = Command::new(RUNNER_BIN)
            .args([SPIKE_PARENT_FLAG, "--", "sleep", "3600"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn runner parent");
        let parent_pid = parent.id() as i32;
        let stdin = parent.stdin.take();
        let (pi, out) = reap.adopt(parent);
        stdin.unwrap().write_all(b"go\n").expect("write go line");
        let line = read_line_bounded(out.unwrap());
        if let Some(unit) = report_str(&line, "holder_unit") {
            reap.units.push(unit.to_string());
        }
        let (holder_pid, child_pid) = pids_of(&line);
        let child = reap.track(child_pid);
        let holder = reap.track(holder_pid);
        let expected = if scope_route_applies() {
            "scope"
        } else {
            "plain"
        };
        assert_eq!(
            report_str(&line, "route"),
            Some(expected),
            "auto must resolve by the observable: {line:?}"
        );

        // SAFETY: SIGKILL to the parent this test spawned.
        assert_eq!(unsafe { libc::kill(parent_pid, libc::SIGKILL) }, 0);
        let _ = reap.children[pi].wait();

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
        let spawned = Command::new(RUNNER_BIN)
            .args([SPIKE_FLAG, "--", "sleep", "3600"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn group-leader holder");
        let group = spawned.id() as i32;
        let (si, out) = reap.adopt(spawned);
        let line = read_line_bounded(out.unwrap());
        let (holder_pid, child_pid) = pids_of(&line);
        let child = reap.track(child_pid);
        let holder = reap.track(holder_pid);
        assert_ne!(
            holder_pid as i32, group,
            "a group leader must hand off to a re-exec'd, setsid()'d holder"
        );

        // The intermediate may already have exited (ESRCH is fine).
        // SAFETY: a negative pid addresses the group this test created.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = reap.children[si].wait();
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
        let holder_proc = Command::new(RUNNER_BIN)
            .args([SPIKE_FLAG, "--", "sleep", "3600"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn holder");
        let (hi, out) = reap.adopt(holder_proc);
        let line = read_line_bounded(out.unwrap());
        let (holder_pid, child_pid) = pids_of(&line);
        let child = reap.track(child_pid);
        assert_eq!(holder_pid, reap.children[hi].id());

        let _ = reap.children[hi].kill(); // SIGKILL the holder
        let _ = reap.children[hi].wait();
        assert!(
            wait_until(Duration::from_secs(10), || !still_same(&child)),
            "PTY child {child:?} outlived its holder — the master close did not hang it up"
        );
    }

    // ── systemd: the killer on a Linux box whose runner is a user service ──
    //
    // `setsid()` leaves a process in its spawner's CGROUP, and a
    // `KillMode=control-group` unit stop (or crash + `Restart=always`) kills
    // the whole cgroup. These two tests model that: the stand-in parent runs
    // as its OWN transient service with `KillMode=control-group`, spawns a
    // holder by an explicit route, and the unit is stopped.

    /// Whether `systemd-run --user` works here. Probed by actually running a
    /// trivial transient scope — presence of the binary is not an answer.
    #[cfg(target_os = "linux")]
    fn user_systemd_usable() -> bool {
        Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    fn cgroup_of(pid: i32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default()
    }

    /// Start the parent as a transient `KillMode=control-group` service that
    /// spawns a holder via `route`, then `systemctl --user stop` that service.
    /// Returns the guard plus the (child, holder) identities recorded BEFORE
    /// the stop. Everything is registered with the guard as it appears.
    #[cfg(target_os = "linux")]
    fn stop_service_parent(route: &str) -> (Reap, Identity, Identity) {
        let mut reap = Reap::default();
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let parent_unit = format!("qontinui-pty-spike-parent-{tag}");
        let report = std::env::temp_dir().join(format!("pty-holder-spike-{tag}.report"));
        // Registered before they exist: stopping an absent unit is harmless.
        reap.units.push(parent_unit.clone());
        reap.files.push(report.clone());

        let status = Command::new("systemd-run")
            .args(["--user", "--quiet", "--collect"])
            .arg(format!("--unit={parent_unit}"))
            .arg("--property=KillMode=control-group")
            .arg("--")
            .arg(RUNNER_BIN)
            .args([SPIKE_PARENT_FLAG, "--route", route, "--report-file"])
            .arg(&report)
            .args(["--", "sleep", "3600"])
            .stdin(Stdio::null())
            .status()
            .expect("run systemd-run for the parent service");
        assert!(status.success(), "systemd-run parent service: {status}");

        assert!(
            wait_until(LINE_TIMEOUT, || report.exists()),
            "parent service {parent_unit} wrote no report file"
        );
        let line = std::fs::read_to_string(&report).unwrap();
        let line = line.trim_end();
        if let Some(unit) = report_str(line, "holder_unit") {
            reap.units.push(unit.to_string());
        }
        let (holder_pid, child_pid) = pids_of(line);
        let child = reap.track(child_pid);
        let holder = reap.track(holder_pid);
        assert_eq!(report_str(line, "route"), Some(route), "{line:?}");

        // Where the holder actually lives decides the outcome; assert it so a
        // pass cannot come from the wrong mechanism.
        let cg = cgroup_of(holder.pid);
        match report_str(line, "holder_unit") {
            Some(unit) => {
                assert!(
                    cg.contains(&format!("{unit}.scope")),
                    "holder not in its scope: {cg}"
                );
                assert!(
                    !cg.contains(&parent_unit),
                    "holder still in the parent's cgroup: {cg}"
                );
            }
            None => assert!(
                cg.contains(&format!("{parent_unit}.service")),
                "plain holder should share the parent's cgroup: {cg}"
            ),
        }

        let stop = Command::new("systemctl")
            .args(["--user", "stop", &parent_unit])
            .stdin(Stdio::null())
            .status()
            .expect("systemctl --user stop");
        assert!(stop.success(), "stopping {parent_unit}: {stop}");
        (reap, child, holder)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_spike_scope_route_survives_stop_of_control_group_service() {
        if !user_systemd_usable() {
            eprintln!(
                "SKIPPED pty_holder_spike_scope_route_survives_stop_of_control_group_service: \
                 `systemd-run --user --scope` is not usable on this box"
            );
            return;
        }
        let (_reap, child, holder) = stop_service_parent("scope");
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            still_same(&child),
            "GO/NO-GO FAILED: PTY child {child:?} did not survive `systemctl --user stop` \
             of its spawner's KillMode=control-group service (scope route)"
        );
        assert!(
            still_same(&holder),
            "holder {holder:?} died with the service"
        );
    }

    /// Negative control: proves the test above can SEE the failure. A plain
    /// (setsid-only) holder stays in the service's cgroup and dies with it.
    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_spike_plain_route_dies_on_stop_of_control_group_service() {
        if !user_systemd_usable() {
            eprintln!(
                "SKIPPED pty_holder_spike_plain_route_dies_on_stop_of_control_group_service: \
                 `systemd-run --user --scope` is not usable on this box"
            );
            return;
        }
        let (_reap, child, _holder) = stop_service_parent("plain");
        assert!(
            wait_until(Duration::from_secs(10), || !still_same(&child)),
            "negative control FAILED: a setsid-only holder's child {child:?} survived a \
             KillMode=control-group stop, so the scope test cannot tell success from failure"
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
            let h = Command::new(RUNNER_BIN)
                .args([SPIKE_FLAG, "--", "sleep", "3600"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn holder");
            let (_, out) = reap.adopt(h);
            let line = read_line_bounded(out.unwrap());
            spawn_ms.push(t.elapsed().as_millis());
            let (hp, cp) = pids_of(&line);
            reap.track(cp);
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
    use std::process::{Child, Command, Stdio};
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

    /// Terminates everything this test spawned on drop, identity-checked.
    #[derive(Default)]
    struct Reap {
        ids: Vec<(u32, u64)>,
        children: Vec<Child>,
    }

    impl Reap {
        fn track(&mut self, pid: u32) -> u64 {
            let (_, created) = probe(pid).expect("probe spawned process");
            self.ids.push((pid, created));
            created
        }
    }

    impl Drop for Reap {
        fn drop(&mut self) {
            for &(pid, created) in &self.ids {
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
            for c in &mut self.children {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    /// Spawn the runner parent (optionally inside an outer job), have it spawn
    /// a holder by `route`, kill the parent, close the job, and report whether
    /// the PTY child survived as the same process.
    fn child_survives(outer_job: Option<bool>, route: &str) -> bool {
        let mut reap = Reap::default();
        let job = outer_job.map(|bok| OuterKillOnCloseJob::create(bok).expect("create job"));
        let mut parent = Command::new(RUNNER_BIN)
            .args([SPIKE_PARENT_FLAG, "--route", route])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn runner parent");
        let stdin = parent.stdin.take();
        let stdout = parent.stdout.take();
        reap.children.push(parent);
        if let Some(j) = &job {
            j.assign(&reap.children[0])
                .expect("assign parent to outer job");
        }
        stdin.unwrap().write_all(b"go\n").expect("write go line");
        let line = read_line_bounded(stdout.unwrap());
        let (holder_pid, child_pid) = pids_of(&line);
        let child_created = reap.track(child_pid);
        reap.track(holder_pid);

        let _ = reap.children[0].kill(); // TerminateProcess
        let _ = reap.children[0].wait();
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
