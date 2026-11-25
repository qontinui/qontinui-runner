//! The active OS process-creation probe (plan
//! `2026-10-19-fleet-is-blind-to-kernel-object-leaks-and-os-process-creation-latency`
//! D3).
//!
//! Spawns a trivial child — Windows `%SystemRoot%\System32\cmd.exe /c exit`,
//! Linux `/bin/true` — and records the wall time from spawn to exit. On
//! degraded spaceship that was 0.7–7 s against tens of ms when healthy, while
//! every other gauge the fleet watched read green.
//!
//! ## Why every probe goes through `run_with_timeout`
//!
//! [`crate::process_helpers::run_with_timeout`] kills the whole process TREE
//! when its bound expires, and reaps it — the tree kill and the reap are
//! pinned by that helper's own tests
//! (`process_helpers::tests::a_timed_out_child_reaps_its_whole_tree_and_leaks_no_reader_threads`
//! and `a_tree_timeout_still_reaps_the_whole_group`), and this module's
//! `a_probe_timeout_is_a_censored_30_000_ms_sample_and_the_child_is_killed`
//! asserts the probe got the kill and the reap back. An abandoned probe child
//! is not a hypothetical: the runner's own resource sampler once piled up
//! **512** stuck `wsl.exe` holding 98k handles by abandoning its probes
//! (`fleet::resource_sample::wsl_probe`). A probe meant to detect a sick
//! kernel must not be what sickens it. The probe runs synchronously on the one
//! `os-health` thread, so exactly one is ever in flight.
//!
//! ## What a result means
//!
//! * A child that exited **successfully** is a latency **sample**.
//! * A child that overran the bound is a **censored sample** at
//!   [`CENSORED_MS`]: it is the worst reading there is, not an absence of one,
//!   so it counts in p50 and max. Dropping it would leave a box whose every
//!   probe times out reading as UNKNOWN — the detector blind on exactly the
//!   worst box (vet defect 6).
//! * A child that could not be created at all (Windows error 1450,
//!   `NoSystemResources`) is **not** a latency sample. It counts in `errors`,
//!   and the latest OS error code rides beside it.
//! * A child that was created but **exited with a failure status** is not a
//!   sample either: `cmd.exe /c exit` and `true` cannot fail on a healthy box,
//!   and the way they do fail on a sick one is `0xC0000142`
//!   (`STATUS_DLL_INIT_FAILED`, the desktop-heap / resource-exhaustion exit),
//!   which is a FAILED process creation, not a fast one. It counts in `errors`
//!   with the exit status as its code.
//! * A probe program that does not exist (`NotFound`) is an INSTRUMENT
//!   failure, not a measurement of the OS: the window is emptied, so the stats
//!   read UNKNOWN (and `kernel_source` stops naming `spawn-probe`) until the
//!   program can be found again. Counting it as a create error would grade a
//!   box with a broken `PATH` as one that cannot create processes.
//!
//! ## Measurement resolution, and what the number includes
//!
//! The reading is wall time around the WHOLE `run_with_timeout` call, so it
//! includes, beyond process creation itself: setting up the two pipe readers,
//! the poll granularity, and on a completed child the bounded pipe drain; on a
//! timeout it includes the tree kill and the reap (a timeout publishes the
//! fixed [`CENSORED_MS`], so that only shows in the log's `elapsed`).
//! `run_with_timeout` polls the child with a backoff that starts at 2 ms and
//! settles at 50 ms, so a reading is rounded UP to the next poll: a 20 ms child
//! reads ~30 ms, and above ~110 ms the error is at most 50 ms. All of that is
//! well inside the S9 bands (250 ms warn, 1,000 ms breach) and is the price of
//! never calling an unbounded `Child::wait`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::process_helpers::TimedOutput;

/// The production bound on one probe.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The value a timed-out probe records: the PRODUCTION bound, in ms.
///
/// Fixed rather than derived from the bound a probe actually ran with, so a
/// censored sample means the same thing everywhere it is read — a test that
/// injects a short bound to finish quickly still records the production value.
pub(crate) const CENSORED_MS: u32 = 30_000;

/// How many probe results the published stats cover (plan D3, vet R7): about
/// ten minutes at one probe a minute, so every 30 s sample carries a full
/// reading.
pub(crate) const WINDOW: usize = 10;

/// One probe's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeResult {
    /// The child was created and exited SUCCESSFULLY on its own; wall time
    /// around the whole bounded run (see the module docs).
    Completed { ms: u32 },
    /// The child overran the bound; its tree was killed and reaped.
    TimedOut,
    /// The child could not be created, or could not be polled. `code` is
    /// `raw_os_error()`.
    SpawnError { code: Option<i32> },
    /// The child was created but exited with a failure status — on Windows
    /// typically `0xC0000142` (`STATUS_DLL_INIT_FAILED`). `code` is the exit
    /// status (`None` for a Unix child killed by a signal).
    ExitedNonZero { code: Option<i32> },
    /// The probe program was not found: an instrument failure, not a
    /// measurement. Empties the window (see [`ProbeWindow::push`]).
    Unavailable,
}

/// What one probe did: the [`ProbeResult`] plus the evidence a caller logs and
/// a test asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeRun {
    pub(crate) result: ProbeResult,
    /// The killed child's pid, on a timeout.
    pub(crate) killed_pid: Option<u32>,
    /// Whether the killed child was reaped, on a timeout.
    pub(crate) reaped: Option<bool>,
    /// Wall time this probe held the calling thread — the whole bounded run,
    /// including pipe-reader setup, the drain and (on a timeout) the tree kill
    /// and the reap.
    pub(crate) elapsed: Duration,
}

/// Builds the probe child. Boxed so a test can point the same probe body at a
/// sleeper or a missing program.
type CommandFactory = Box<dyn Fn() -> std::process::Command + Send>;

/// The probe: what to spawn and how long to wait for it.
pub(crate) struct SpawnProbe {
    command: CommandFactory,
    timeout: Duration,
}

impl SpawnProbe {
    /// The D3 probe: `cmd.exe /c exit` or `/bin/true`, bounded at
    /// [`PROBE_TIMEOUT`].
    pub(crate) fn production() -> Self {
        Self {
            command: Box::new(probe_command),
            timeout: PROBE_TIMEOUT,
        }
    }

    /// A probe with an injected command and bound — tests only.
    #[cfg(test)]
    pub(crate) fn with(
        command: impl Fn() -> std::process::Command + Send + 'static,
        timeout: Duration,
    ) -> Self {
        Self {
            command: Box::new(command),
            timeout,
        }
    }

    /// Run one probe, synchronously, on the calling thread.
    pub(crate) fn run_once(&self) -> ProbeRun {
        let cmd = (self.command)();
        let started = Instant::now();
        let out = crate::process_helpers::run_with_timeout(cmd, self.timeout);
        let elapsed = started.elapsed();
        let (result, killed_pid, reaped) = match out {
            Ok(TimedOutput::Completed(o)) if o.status.success() => (
                ProbeResult::Completed {
                    ms: u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX),
                },
                None,
                None,
            ),
            Ok(TimedOutput::Completed(o)) => (
                ProbeResult::ExitedNonZero {
                    code: o.status.code(),
                },
                None,
                None,
            ),
            Ok(TimedOutput::TimedOut { pid, reaped }) => {
                (ProbeResult::TimedOut, Some(pid), Some(reaped))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (ProbeResult::Unavailable, None, None)
            }
            Err(e) => (
                ProbeResult::SpawnError {
                    code: e.raw_os_error(),
                },
                None,
                None,
            ),
        };
        ProbeRun {
            result,
            killed_pid,
            reaped,
            elapsed,
        }
    }
}

/// `%SystemRoot%\System32\cmd.exe /c exit`, by absolute path so the probe
/// times process CREATION rather than a `PATH` search. Falls back to
/// [`crate::process_helpers::cmd_no_window`] when `SystemRoot` is unset.
#[cfg(windows)]
pub(crate) fn probe_command() -> std::process::Command {
    let mut cmd = match std::env::var_os("SystemRoot") {
        Some(root) if !root.is_empty() => crate::process_helpers::no_window(
            std::path::Path::new(&root).join("System32").join("cmd.exe"),
        ),
        _ => crate::process_helpers::cmd_no_window(),
    };
    cmd.args(["/c", "exit"]);
    cmd
}

/// `true`, by absolute path: `/bin/true`, else `/usr/bin/true` (a usr-merged
/// or minimal distribution may have only one), else a bare `true` resolved
/// through `PATH`. If none exists the spawn fails `NotFound`, which the probe
/// reports as an instrument failure ([`ProbeResult::Unavailable`]).
#[cfg(not(windows))]
pub(crate) fn probe_command() -> std::process::Command {
    crate::process_helpers::no_window(true_program(|p| p.exists()))
}

/// The `true` program to run, given which paths exist. PURE apart from
/// `exists`, so the fallback order is testable on any OS.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn true_program(exists: impl Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|p| exists(p))
        .unwrap_or_else(|| std::path::PathBuf::from("true"))
}

/// The published reading over the last [`WINDOW`] probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpawnProbeStats {
    /// Median of the samples (exits plus censored timeouts), nearest-rank:
    /// the `ceil(n/2)`-th smallest. `None` when the window holds only errors.
    pub(crate) p50_ms: Option<u32>,
    /// Largest sample. `None` when the window holds only errors.
    pub(crate) max_ms: Option<u32>,
    /// Probes in the window, of every outcome (≤ [`WINDOW`]).
    pub(crate) count: u32,
    /// Censored samples in the window.
    pub(crate) timeouts: u32,
    /// Probes in the window whose child could not be created, or was created
    /// and exited with a failure status.
    pub(crate) errors: u32,
    /// The code of the LATEST error in the window, when it carried one: the
    /// OS error for a create failure, the exit status for a child that exited
    /// failing (e.g. `0xC0000142` as an `i32`). `None` once every error has
    /// aged out.
    pub(crate) error_code: Option<i32>,
}

impl SpawnProbeStats {
    /// Probes in the window whose child exited on its own. Zero alongside a
    /// non-zero `count` is S9's `spawn_failed` shape (plan D4).
    pub(crate) fn successes(&self) -> u32 {
        self.count
            .saturating_sub(self.timeouts)
            .saturating_sub(self.errors)
    }
}

/// The rolling window of the last [`WINDOW`] probe results.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProbeWindow {
    results: VecDeque<ProbeResult>,
}

impl ProbeWindow {
    /// Add one result, dropping the oldest once the window is full.
    ///
    /// [`ProbeResult::Unavailable`] instead EMPTIES the window: the instrument
    /// is broken, so the stats must read UNKNOWN rather than keep serving the
    /// last measurements as if they were current.
    pub(crate) fn push(&mut self, r: ProbeResult) {
        if r == ProbeResult::Unavailable {
            self.results.clear();
            return;
        }
        if self.results.len() == WINDOW {
            self.results.pop_front();
        }
        self.results.push_back(r);
    }

    /// The stats over the window, or `None` before the first probe.
    pub(crate) fn stats(&self) -> Option<SpawnProbeStats> {
        if self.results.is_empty() {
            return None;
        }
        let mut samples: Vec<u32> = self
            .results
            .iter()
            .filter_map(|r| match r {
                ProbeResult::Completed { ms } => Some(*ms),
                ProbeResult::TimedOut => Some(CENSORED_MS),
                ProbeResult::SpawnError { .. }
                | ProbeResult::ExitedNonZero { .. }
                | ProbeResult::Unavailable => None,
            })
            .collect();
        samples.sort_unstable();
        let count = |pred: fn(&ProbeResult) -> bool| -> u32 {
            u32::try_from(self.results.iter().filter(|r| pred(r)).count()).unwrap_or(u32::MAX)
        };
        let is_error = |r: &ProbeResult| {
            matches!(
                r,
                ProbeResult::SpawnError { .. } | ProbeResult::ExitedNonZero { .. }
            )
        };
        let error_code = self.results.iter().rev().find_map(|r| match r {
            ProbeResult::SpawnError { code } | ProbeResult::ExitedNonZero { code } => Some(*code),
            _ => None,
        });
        Some(SpawnProbeStats {
            p50_ms: samples.get(samples.len().saturating_sub(1) / 2).copied(),
            max_ms: samples.last().copied(),
            count: count(|_| true),
            timeouts: count(|r| matches!(r, ProbeResult::TimedOut)),
            errors: count(is_error),
            error_code: error_code.flatten(),
        })
    }
}

/// A child that runs far longer than any bound a test injects (~60 s).
#[cfg(test)]
pub(crate) fn sleeper() -> std::process::Command {
    #[cfg(windows)]
    {
        let mut c = crate::process_helpers::cmd_no_window();
        c.args(["/C", "ping -n 60 127.0.0.1"]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = crate::process_helpers::no_window("sh");
        c.args(["-c", "sleep 60"]);
        c
    }
}

/// A file that exists but is not an executable: a real create failure that is
/// not `NotFound`. Written once per test process, under the temp dir.
#[cfg(test)]
pub(crate) fn not_an_executable() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("os-health-not-an-exe-{}.exe", std::process::id()));
    if !p.exists() {
        std::fs::write(&p, b"this is not an executable image").expect("write the fake exe");
    }
    p
}

/// The OS error creating [`not_an_executable`] fails with:
/// ERROR_EXE_MACHINE_TYPE_MISMATCH (216, a file too short to carry a PE
/// header) on Windows, EACCES (no execute bit) on Linux.
#[cfg(test)]
pub(crate) const NOT_AN_EXE_SPAWN_ERROR: i32 = if cfg!(windows) { 216 } else { 13 };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_censored_value_is_the_production_bound() {
        assert_eq!(u128::from(CENSORED_MS), PROBE_TIMEOUT.as_millis());
    }

    #[test]
    fn the_production_probe_completes_and_is_a_sample() {
        let run = SpawnProbe::production().run_once();
        match run.result {
            ProbeResult::Completed { ms } => assert!(ms < CENSORED_MS, "{ms} ms"),
            other => panic!("the trivial child must exit on its own, got {other:?}"),
        }
        assert_eq!((run.killed_pid, run.reaped), (None, None));
    }

    #[test]
    fn a_probe_timeout_is_a_censored_30_000_ms_sample_and_the_child_is_killed() {
        let budget = Duration::from_millis(300);
        let run = SpawnProbe::with(sleeper, budget).run_once();
        assert_eq!(run.result, ProbeResult::TimedOut);
        assert!(run.killed_pid.is_some_and(|p| p > 0), "{run:?}");
        assert_eq!(run.reaped, Some(true), "the killed child must be reaped");
        // The bound held: the thread got back long before the 60 s child.
        assert!(run.elapsed < Duration::from_secs(10), "{:?}", run.elapsed);

        let mut w = ProbeWindow::default();
        w.push(run.result);
        assert_eq!(
            w.stats(),
            Some(SpawnProbeStats {
                p50_ms: Some(30_000),
                max_ms: Some(30_000),
                count: 1,
                timeouts: 1,
                errors: 0,
                error_code: None,
            })
        );
        assert_eq!(w.stats().unwrap().successes(), 0);
    }

    #[test]
    fn a_spawn_error_counts_in_errors_with_its_os_code_and_is_not_a_sample() {
        let bad = not_an_executable();
        let run = SpawnProbe::with(
            move || crate::process_helpers::no_window(&bad),
            Duration::from_secs(5),
        )
        .run_once();
        assert_eq!(
            run.result,
            ProbeResult::SpawnError {
                code: Some(NOT_AN_EXE_SPAWN_ERROR)
            }
        );

        let mut w = ProbeWindow::default();
        w.push(run.result);
        assert_eq!(
            w.stats(),
            Some(SpawnProbeStats {
                p50_ms: None,
                max_ms: None,
                count: 1,
                timeouts: 0,
                errors: 1,
                error_code: Some(NOT_AN_EXE_SPAWN_ERROR),
            })
        );
    }

    /// A child that exits failing is a failed process creation, never a fast
    /// healthy sample (`0xC0000142` is how `cmd /c exit` fails on a box out of
    /// desktop heap). Exit status 7 stands in for it.
    #[test]
    fn a_non_zero_exit_counts_in_errors_with_its_exit_code_and_is_not_a_sample() {
        let failing = || {
            #[cfg(windows)]
            {
                let mut c = crate::process_helpers::cmd_no_window();
                c.args(["/c", "exit 7"]);
                c
            }
            #[cfg(not(windows))]
            {
                let mut c = crate::process_helpers::no_window("sh");
                c.args(["-c", "exit 7"]);
                c
            }
        };
        let run = SpawnProbe::with(failing, Duration::from_secs(10)).run_once();
        assert_eq!(run.result, ProbeResult::ExitedNonZero { code: Some(7) });

        let mut w = ProbeWindow::default();
        w.push(ProbeResult::Completed { ms: 20 });
        w.push(run.result);
        let s = w.stats().unwrap();
        assert_eq!((s.count, s.errors, s.error_code), (2, 1, Some(7)));
        assert_eq!(
            (s.p50_ms, s.max_ms),
            (Some(20), Some(20)),
            "the failed exit is not a sample"
        );
        assert_eq!(s.successes(), 1);
    }

    /// A probe program that does not exist is an instrument failure: the
    /// stats go UNKNOWN, and it is NOT counted as the OS failing to create a
    /// process (which S9 would grade `spawn_failed`).
    #[test]
    fn a_missing_probe_program_is_an_instrument_failure_not_a_measurement() {
        let missing = std::env::temp_dir().join("os-health-probe-missing-0e1f.exe");
        assert!(!missing.exists());
        let run = SpawnProbe::with(
            move || crate::process_helpers::no_window(&missing),
            Duration::from_secs(5),
        )
        .run_once();
        assert_eq!(run.result, ProbeResult::Unavailable);

        let mut w = ProbeWindow::default();
        for _ in 0..3 {
            w.push(ProbeResult::Completed { ms: 20 });
        }
        w.push(run.result);
        assert_eq!(w.stats(), None, "a broken instrument empties the window");
        w.push(ProbeResult::Completed { ms: 25 });
        assert_eq!(w.stats().unwrap().count, 1);
    }

    #[test]
    fn the_true_program_falls_back_from_bin_to_usr_bin_to_path() {
        use std::path::{Path, PathBuf};
        assert_eq!(true_program(|_| true), PathBuf::from("/bin/true"));
        assert_eq!(
            true_program(|p| p == Path::new("/usr/bin/true")),
            PathBuf::from("/usr/bin/true")
        );
        assert_eq!(true_program(|_| false), PathBuf::from("true"));
    }

    #[test]
    fn the_window_keeps_ten_and_drops_the_oldest() {
        let mut w = ProbeWindow::default();
        assert_eq!(w.stats(), None);
        for ms in 1..=12 {
            w.push(ProbeResult::Completed { ms });
        }
        // 1 and 2 dropped; 3..=12 remain.
        let s = w.stats().unwrap();
        assert_eq!(s.count, 10);
        assert_eq!(s.max_ms, Some(12));
        // Nearest-rank median of 3..=12: the 5th smallest.
        assert_eq!(s.p50_ms, Some(7));
        assert_eq!(s.successes(), 10);

        // An error ages out of the window with its code.
        let mut w = ProbeWindow::default();
        w.push(ProbeResult::SpawnError { code: Some(1450) });
        for _ in 0..9 {
            w.push(ProbeResult::Completed { ms: 20 });
        }
        assert_eq!(w.stats().unwrap().error_code, Some(1450));
        w.push(ProbeResult::Completed { ms: 20 });
        let s = w.stats().unwrap();
        assert_eq!((s.errors, s.error_code, s.count), (0, None, 10));
    }

    #[test]
    fn a_mixed_window_reports_each_outcome() {
        let mut w = ProbeWindow::default();
        w.push(ProbeResult::Completed { ms: 2_400 });
        w.push(ProbeResult::TimedOut);
        w.push(ProbeResult::SpawnError { code: Some(1450) });
        w.push(ProbeResult::Completed { ms: 700 });
        let s = w.stats().unwrap();
        // Samples 700, 2_400, 30_000: the censored timeout raises the median.
        assert_eq!(s.p50_ms, Some(2_400));
        assert_eq!(s.max_ms, Some(30_000));
        assert_eq!((s.count, s.timeouts, s.errors), (4, 1, 1));
        assert_eq!(s.error_code, Some(1450));
        assert_eq!(s.successes(), 2);
        // A later failing exit is the latest error code.
        w.push(ProbeResult::ExitedNonZero {
            code: Some(0xC000_0142_u32 as i32),
        });
        let s = w.stats().unwrap();
        assert_eq!((s.count, s.errors), (5, 2));
        assert_eq!(s.error_code, Some(0xC000_0142_u32 as i32));
    }
}
