//! The bash identity shim launches only a SETTLED `claude` (plan
//! `2026-11-10-shim-settle-wait-and-in-memory-launch-liveness` Phase 3, D3),
//! proven by RUNNING the rendered template.
//!
//! Every run's PATH is the shim's own dir, a scratch bin dir and the fake
//! real-tool dir — never `/usr/bin` or `/bin` — so a system `claude` or `stat`
//! can never answer for the one a test scripts. The scratch bin holds the real
//! `sleep`, a `curl` that records each beacon it is asked to send, and, per
//! test, a `stat` whose answers a control file scripts (or the real `stat`, or
//! none at all).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::shim_test_support::{identity_shim_cmd, write_exe, EXE_LOCK};

const BUDGET: Duration = Duration::from_secs(40);

/// The line the shim prints once while a resolved `claude` is still changing.
const WAIT_LINE_TAIL: &str = "(an install may still be writing it); waiting up to";
/// The line the shim prints when the bound runs out on a changing `claude`.
const LAUNCH_ANYWAY_TAIL: &str = "launching it anyway";

fn system_tool(tool: &str) -> Option<PathBuf> {
    ["/bin", "/usr/bin", "/usr/local/bin"]
        .iter()
        .map(|d| Path::new(d).join(tool))
        .find(|p| p.exists())
}

/// Which `stat` the scratch bin holds.
#[derive(Clone, Copy)]
enum Stat {
    /// No `stat` on PATH at all.
    Absent,
    /// A GNU-shaped fake: `-c %Z` answers from the control file.
    Gnu,
    /// A BSD-shaped fake: `-c` is a usage error on stderr with exit 1, and
    /// `-f %c` answers from the control file.
    Bsd,
    /// The system's own `stat`.
    Real,
}

/// What the real-tool dir holds when the shim starts.
#[derive(Clone, Copy)]
enum Claude {
    /// A fake `claude` that records it ran.
    Present,
    /// Nothing, until the shim's first wait-loop `sleep` renames it in.
    AppearsOnFirstSleep,
}

struct Scenario<'a> {
    stat: Stat,
    /// One answer per answering `stat` call; the last repeats. `old` is epoch
    /// second 1, `now` the current second, `future` 100 s ahead, anything else
    /// is printed verbatim (a literal epoch, or garbage).
    answers: &'a [&'a str],
    wait: &'a str,
    claude: Claude,
    /// Unset `EPOCHSECONDS` (via `BASH_ENV`) and put a `date` on PATH that
    /// prints this epoch, so the shim takes its bash-3.2 `date +%s` fallback.
    date_fallback: Option<u64>,
}

impl Default for Scenario<'_> {
    fn default() -> Self {
        Self {
            stat: Stat::Gnu,
            answers: &["old"],
            wait: "10",
            claude: Claude::Present,
            date_fallback: None,
        }
    }
}

struct Run {
    ok: bool,
    stderr: String,
    ran: bool,
    elapsed: Duration,
    /// The argv of every `stat` call, one per line.
    stat_calls: String,
    /// Every beacon `curl` was asked to send, one argv per line.
    beacons: String,
    /// One line per call of the fake `date`.
    date_calls: String,
    /// The real `claude`'s change time (its target's, through a link) after
    /// the run, and when the fake `claude` ran (its marker's mtime).
    claude_changed: Option<SystemTime>,
    launched_at: Option<SystemTime>,
    /// When the shim first called `stat`, under [`Stat::Real`] only.
    first_stat_at: Option<SystemTime>,
}

/// A file's change time, following links (what the shim's `stat -L` reads).
fn ctime_of(path: &Path) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some(
        SystemTime::UNIX_EPOCH
            + Duration::new(
                u64::try_from(meta.ctime()).ok()?,
                u32::try_from(meta.ctime_nsec()).ok()?,
            ),
    )
}

/// The system `stat` behind a wrapper that first logs the call's wall-clock
/// time in nanoseconds to `<root>/stat.times`, so a test can tell whether the
/// shim's first probe came early enough that a wait line was owed.
fn timed_real_stat(bin: &Path, root: &Path) {
    let stat = system_tool("stat").expect("a system `stat`");
    let date = system_tool("date").expect("a system `date`");
    write_exe(
        &bin.join("stat"),
        &format!(
            "#!/bin/sh\n'{date}' +%s%N >> '{times}'\nexec '{stat}' \"$@\"\n",
            date = date.display(),
            times = root.join("stat.times").display(),
            stat = stat.display(),
        ),
    );
}

fn first_stat_at(root: &Path) -> Option<SystemTime> {
    let ns: u64 = read(&root.join("stat.times")).lines().next()?.trim().parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(ns))
}

/// The real-clock guarantee, measured from the WRITE rather than from when the
/// harness started the shim: the fake `claude` ran at least 2 s after its
/// target last changed. The wait line is owed whenever the shim's first probe
/// came under 2 s after that change (whole-second `now - ctime` is then at
/// most 2, under the shim's 3); a probe that a loaded box delayed past that may
/// legitimately find the target settled and print nothing.
fn assert_launched_two_seconds_after_the_write(
    changed: Option<SystemTime>,
    launched: Option<SystemTime>,
    first_stat: Option<SystemTime>,
    stderr: &str,
) {
    let changed = changed.expect("the real claude's change time");
    let launched = launched.expect("the fake claude ran and left its marker");
    let gap = launched.duration_since(changed).unwrap_or_default();
    assert!(
        gap >= Duration::from_secs(2),
        "the claude launched {gap:?} after its target changed: {stderr}"
    );
    let first_stat = first_stat.expect("the shim called stat");
    if first_stat < changed + Duration::from_secs(2) {
        assert!(
            stderr.contains(WAIT_LINE_TAIL),
            "the first probe came {:?} after the write, so the wait line is owed: {stderr}",
            first_stat.duration_since(changed).unwrap_or_default()
        );
    } else {
        eprintln!(
            "NOTE: the shim's first stat came {:?} after the write; the wait line is not owed",
            first_stat.duration_since(changed).unwrap_or_default()
        );
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn fake_stat(bin: &Path, root: &Path, bsd: bool, answers: &[&str]) {
    let date = system_tool("date").expect("a system `date` for the fake stat's clock");
    let (ctl, count, log) = (
        root.join("stat.ctl"),
        root.join("stat.count"),
        root.join("stat.log"),
    );
    std::fs::write(&ctl, format!("{}\n", answers.join("\n"))).unwrap();
    write_exe(
        &bin.join("stat"),
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ '{bsd}' = 1 ]; then
  for a in "$@"; do
    if [ "$a" = "-c" ]; then
      echo "stat: illegal option -- c" >&2
      echo "usage: stat [-FLnq] [-f format | -l | -r | -s | -x] [-t timefmt] [file ...]" >&2
      exit 1
    fi
  done
fi
n=0
[ -f '{count}' ] && read n < '{count}'
n=$((n + 1))
echo "$n" > '{count}'
i=0
v=''
while IFS= read -r line; do
  i=$((i + 1))
  v="$line"
  [ "$i" -ge "$n" ] && break
done < '{ctl}'
now=$('{date}' +%s)
case "$v" in
  old) echo 1 ;;
  now) echo "$now" ;;
  future) echo $((now + 100)) ;;
  *) echo "$v" ;;
esac
"#,
            log = log.display(),
            bsd = u8::from(bsd),
            count = count.display(),
            ctl = ctl.display(),
            date = date.display(),
        ),
    );
}

fn fake_claude_body(marker: &Path) -> String {
    format!("#!/bin/sh\necho ran > '{}'\nexit 0\n", marker.display())
}

/// Run `claude --version` through the identity shim under `scenario`.
fn run(scenario: &Scenario) -> Run {
    let real_sleep = system_tool("sleep").expect("a system `sleep`");
    let _serial = EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (shim_dir, bin, real_dir) = (root.join("shim"), root.join("bin"), root.join("real"));
    for d in [&shim_dir, &bin, &real_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (marker, beacons, date_log) = (root.join("ran"), root.join("beacons"), root.join("date.log"));
    write_exe(
        &bin.join("curl"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", beacons.display()),
    );
    match scenario.stat {
        Stat::Absent => {}
        Stat::Gnu => fake_stat(&bin, root, false, scenario.answers),
        Stat::Bsd => fake_stat(&bin, root, true, scenario.answers),
        Stat::Real => timed_real_stat(&bin, root),
    }
    let real = real_dir.join("claude");
    match scenario.claude {
        Claude::Present => {
            symlink(&real_sleep, bin.join("sleep")).unwrap();
            write_exe(&real, &fake_claude_body(&marker));
        }
        Claude::AppearsOnFirstSleep => {
            let mv = system_tool("mv").expect("a system `mv`");
            let staged = root.join("claude.staged");
            write_exe(&staged, &fake_claude_body(&marker));
            write_exe(
                &bin.join("sleep"),
                &format!(
                    "#!/bin/sh\n[ -e '{staged}' ] && '{mv}' -f '{staged}' '{real}'\nexec '{sleep}' \"$@\"\n",
                    mv = mv.display(),
                    staged = staged.display(),
                    real = real.display(),
                    sleep = real_sleep.display(),
                ),
            );
        }
    }
    let path = format!(
        "{}:{}:{}",
        shim_dir.display(),
        bin.display(),
        real_dir.display()
    );
    let mut cmd = identity_shim_cmd(&shim_dir, &["--version"], &path, scenario.wait);
    cmd.env("QONTINUI_INSTALL_INTERCEPT_PORT", "1");
    if let Some(epoch) = scenario.date_fallback {
        let bash_env = root.join("bash_env");
        std::fs::write(&bash_env, "unset EPOCHSECONDS\n").unwrap();
        cmd.env("BASH_ENV", &bash_env);
        write_exe(
            &bin.join("date"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\necho {epoch}\n",
                date_log.display()
            ),
        );
    }
    let begun = Instant::now();
    let out = crate::process_helpers::output_with_timeout(cmd, BUDGET).unwrap();
    Run {
        ok: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ran: marker.exists(),
        elapsed: begun.elapsed(),
        stat_calls: read(&root.join("stat.log")),
        beacons: read(&beacons),
        date_calls: read(&date_log),
        claude_changed: ctime_of(&real),
        launched_at: std::fs::metadata(&marker).and_then(|m| m.modified()).ok(),
        first_stat_at: first_stat_at(root),
    }
}

/// A launch that resolved something posts `invoked` and no launch failure.
fn assert_invoked_only(run: &Run, what: &str) {
    assert!(
        run.beacons.contains(r#""event":"invoked""#),
        "{what}: the invoked beacon is sent: {}",
        run.beacons
    );
    assert!(
        !run.beacons.contains("resolve-failed") && !run.beacons.contains("exec-failed"),
        "{what}: a launch that resolved something is no launch failure: {}",
        run.beacons
    );
}

#[test]
fn a_settled_claude_launches_at_once_with_nothing_on_stderr() {
    let run = run(&Scenario::default());
    assert!(run.ok && run.ran, "the real claude ran: {}", run.stderr);
    assert_eq!(run.stderr, "", "TRANSPARENT: a settled hit adds no output");
    assert_eq!(
        run.stat_calls.lines().count(),
        1,
        "one stat, with -L: {}",
        run.stat_calls
    );
    assert!(run.stat_calls.starts_with("-L -c %Z "), "{}", run.stat_calls);
    assert_invoked_only(&run, "settled");
}

#[test]
fn a_changing_claude_is_announced_once_and_launched_once_settled() {
    let run = run(&Scenario {
        answers: &["now", "now", "now", "old"],
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "the real claude ran: {}", run.stderr);
    assert_eq!(
        run.stderr.matches(WAIT_LINE_TAIL).count(),
        1,
        "one wait line: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("/real/claude changed 0s ago (an install may still be writing it); waiting up to 10s for it to settle"),
        "the wait line names the path, the age and the bound: {}",
        run.stderr
    );
    assert!(!run.stderr.contains(LAUNCH_ANYWAY_TAIL), "{}", run.stderr);
    assert_eq!(run.stat_calls.lines().count(), 4, "{}", run.stat_calls);
    assert_invoked_only(&run, "changing then settled");
}

#[test]
fn a_claude_still_changing_at_the_bound_is_launched_anyway() {
    let run = run(&Scenario {
        answers: &["now"],
        wait: "1",
        ..Scenario::default()
    });
    assert!(run.ok, "exit 0 from the fake real claude: {}", run.stderr);
    assert!(run.ran, "the changing candidate is launched: {}", run.stderr);
    assert!(
        run.stderr.contains("/real/claude has not settled after 1s; launching it anyway"),
        "{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("was not found on PATH"),
        "no give-up line: something launchable resolved: {}",
        run.stderr
    );
    assert_invoked_only(&run, "changing past the bound");
}

#[test]
fn an_unreadable_change_time_adds_no_wait() {
    for (stat, answers, what) in [
        (Stat::Absent, &["old"][..], "no stat on PATH"),
        (Stat::Gnu, &["12abc"][..], "garbled stat output"),
    ] {
        let run = run(&Scenario {
            stat,
            answers,
            ..Scenario::default()
        });
        assert!(run.ok && run.ran, "{what}: the real claude ran: {}", run.stderr);
        assert_eq!(run.stderr, "", "{what}: unknown counts as settled");
        assert!(
            run.elapsed < Duration::from_secs(5),
            "{what}: no settle wait ({:?})",
            run.elapsed
        );
    }
}

#[test]
fn a_bsd_stat_is_read_through_dash_f_and_its_usage_error_never_reaches_the_pane() {
    let run_old = run(&Scenario {
        stat: Stat::Bsd,
        answers: &["old"],
        ..Scenario::default()
    });
    assert!(run_old.ok && run_old.ran, "{}", run_old.stderr);
    assert_eq!(
        run_old.stderr, "",
        "BSD stat's `-c` usage error must not reach the pane"
    );
    assert!(
        run_old.stat_calls.contains("-L -c %Z ") && run_old.stat_calls.contains("-L -f %c "),
        "both probes ran, both with -L: {}",
        run_old.stat_calls
    );

    let run_fresh = run(&Scenario {
        stat: Stat::Bsd,
        answers: &["now", "old"],
        ..Scenario::default()
    });
    assert!(run_fresh.ok && run_fresh.ran, "{}", run_fresh.stderr);
    assert_eq!(
        run_fresh.stderr.lines().count(),
        1,
        "exactly the one wait line: {}",
        run_fresh.stderr
    );
    assert!(run_fresh.stderr.contains(WAIT_LINE_TAIL), "{}", run_fresh.stderr);
}

#[test]
fn a_change_time_in_the_future_waits_to_the_bound_then_launches() {
    let run = run(&Scenario {
        answers: &["future"],
        wait: "1",
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "{}", run.stderr);
    assert!(
        run.stderr.contains("/real/claude changed 0s ago"),
        "a future change time reads as changing, age 0: {}",
        run.stderr
    );
    assert!(run.stderr.contains(LAUNCH_ANYWAY_TAIL), "{}", run.stderr);
}

#[test]
fn a_zero_wait_disables_the_settle_wait() {
    let run = run(&Scenario {
        answers: &["now"],
        wait: "0",
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "{}", run.stderr);
    assert_eq!(run.stderr, "", "no wait, no line");
    assert_eq!(run.stat_calls, "", "a zero wait never stats");
}

#[test]
fn absent_then_changing_then_settled_share_one_bound() {
    let run = run(&Scenario {
        answers: &["now", "old"],
        claude: Claude::AppearsOnFirstSleep,
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "{}", run.stderr);
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(lines.len(), 2, "one miss line, one settle line: {}", run.stderr);
    assert!(lines[0].contains("claude not found on PATH"), "{}", run.stderr);
    assert!(lines[1].contains(WAIT_LINE_TAIL), "{}", run.stderr);
    assert_invoked_only(&run, "absent, changing, settled");
}

/// bash older than 5 (macOS's 3.2) has no `EPOCHSECONDS`; the shim's clock is
/// then `date +%s`. `BASH_ENV` unsets the builtin before the shim runs, and a
/// fake `date` prints a fixed epoch, so the ages below come from it alone.
#[test]
fn without_epochseconds_the_clock_falls_back_to_date() {
    let run = run(&Scenario {
        answers: &["998", "990"],
        date_fallback: Some(1_000),
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "{}", run.stderr);
    assert!(
        run.stderr.contains("/real/claude changed 2s ago"),
        "age 1000 - 998 = 2 s is still changing: {}",
        run.stderr
    );
    assert!(!run.stderr.contains(LAUNCH_ANYWAY_TAIL), "{}", run.stderr);
    assert_eq!(
        run.date_calls.lines().collect::<Vec<_>>(),
        ["+%s", "+%s"],
        "one `date +%s` per settle check"
    );
}

/// The real clock and the real `stat`: a fake real `claude` written just before
/// the run starts only once at least 2 s have passed since that write,
/// measured on the filesystem's own clock (its ctime against the marker the
/// fake writes when it runs).
#[test]
fn with_the_real_stat_a_freshly_written_claude_waits_at_least_two_seconds() {
    if system_tool("stat").is_none() {
        eprintln!("SKIP: no system stat");
        return;
    }
    let run = run(&Scenario {
        stat: Stat::Real,
        ..Scenario::default()
    });
    assert!(run.ok && run.ran, "{}", run.stderr);
    assert_launched_two_seconds_after_the_write(
        run.claude_changed,
        run.launched_at,
        run.first_stat_at,
        &run.stderr,
    );
}

/// `-L`: a `bin/claude` symlink made long ago, pointing at a target rewritten
/// just now, must wait — the LINK's own change time is settled, the target's
/// is not.
#[test]
fn with_the_real_stat_an_old_symlink_to_a_fresh_target_waits() {
    let (Some(_), Some(real_sleep)) = (system_tool("stat"), system_tool("sleep")) else {
        eprintln!("SKIP: no system stat or sleep");
        return;
    };
    let _serial = EXE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (shim_dir, bin, real_dir) = (root.join("shim"), root.join("bin"), root.join("real"));
    for d in [&shim_dir, &bin, &real_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let marker = root.join("ran");
    let target = root.join("claude-image");
    write_exe(&target, &fake_claude_body(&marker));
    symlink(&target, real_dir.join("claude")).unwrap();
    timed_real_stat(&bin, root);
    symlink(&real_sleep, bin.join("sleep")).unwrap();
    // Age the LINK past the shim's 3 whole seconds, then rewrite the target.
    std::thread::sleep(Duration::from_millis(3_300));
    write_exe(&target, &fake_claude_body(&marker));
    let path = format!(
        "{}:{}:{}",
        shim_dir.display(),
        bin.display(),
        real_dir.display()
    );
    let out = crate::process_helpers::output_with_timeout(
        identity_shim_cmd(&shim_dir, &["--version"], &path, "10"),
        BUDGET,
    )
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && marker.exists(), "{stderr}");
    // Read through the LINK: what the shim's `stat -L` saw is the target's
    // fresh change time, not the link's 3 s-old one.
    assert_launched_two_seconds_after_the_write(
        ctime_of(&real_dir.join("claude")),
        std::fs::metadata(&marker).and_then(|m| m.modified()).ok(),
        first_stat_at(root),
        &stderr,
    );
}
