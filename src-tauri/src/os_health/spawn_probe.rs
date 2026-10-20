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
//! when its bound expires, and reaps it. An abandoned probe child is not a
//! hypothetical: the runner's own resource sampler once piled up **512** stuck
//! `wsl.exe` holding 98k handles by abandoning its probes
//! (`fleet::resource_sample::wsl_probe`). A probe meant to detect a sick
//! kernel must not be what sickens it. The probe runs synchronously on the one
//! `os-health` thread, so exactly one is ever in flight.
//!
//! ## What a result means
//!
//! * A child that exited is a latency **sample**.
//! * A child that overran the bound is a **censored sample** at
//!   [`CENSORED_MS`]: it is the worst reading there is, not an absence of one,
//!   so it counts in p50 and max. Dropping it would leave a box whose every
//!   probe times out reading as UNKNOWN — the detector blind on exactly the
//!   worst box (vet defect 6).
//! * A child that could not be created at all (Windows error 1450,
//!   `NoSystemResources`) is **not** a latency sample. It counts in `errors`,
//!   and the latest OS error code rides beside it.
//!
//! ## Measurement resolution
//!
//! `run_with_timeout` polls the child with a backoff that starts at 2 ms and
//! settles at 50 ms, so a reading is rounded UP to the next poll: a 20 ms child
//! reads ~30 ms, and above ~110 ms the error is at most 50 ms. That is well
//! inside the S9 bands (250 ms warn, 1,000 ms breach) and is the price of never
//! calling an unbounded `Child::wait`.

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
    /// The child was created and exited on its own; wall time spawn → exit.
    Completed { ms: u32 },
    /// The child overran the bound; its tree was killed and reaped.
    TimedOut,
    /// The child could not be created, or could not be polled. `code` is
    /// `raw_os_error()`.
    SpawnError { code: Option<i32> },
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
    /// Wall time this probe held the calling thread.
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
            Ok(TimedOutput::Completed(_)) => (
                ProbeResult::Completed {
                    ms: u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX),
                },
                None,
                None,
            ),
            Ok(TimedOutput::TimedOut { pid, reaped }) => {
                (ProbeResult::TimedOut, Some(pid), Some(reaped))
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

/// `/bin/true`.
#[cfg(not(windows))]
pub(crate) fn probe_command() -> std::process::Command {
    crate::process_helpers::no_window("/bin/true")
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
    /// Probes in the window whose child could not be created.
    pub(crate) errors: u32,
    /// The OS error code of the LATEST error in the window, when it carried
    /// one. `None` once every error has aged out.
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
    pub(crate) fn push(&mut self, r: ProbeResult) {
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
                ProbeResult::SpawnError { .. } => None,
            })
            .collect();
        samples.sort_unstable();
        let count = |pred: fn(&ProbeResult) -> bool| -> u32 {
            u32::try_from(self.results.iter().filter(|r| pred(r)).count()).unwrap_or(u32::MAX)
        };
        let error_code = self.results.iter().rev().find_map(|r| match r {
            ProbeResult::SpawnError { code } => Some(*code),
            _ => None,
        });
        Some(SpawnProbeStats {
            p50_ms: samples.get(samples.len().saturating_sub(1) / 2).copied(),
            max_ms: samples.last().copied(),
            count: count(|_| true),
            timeouts: count(|r| matches!(r, ProbeResult::TimedOut)),
            errors: count(|r| matches!(r, ProbeResult::SpawnError { .. })),
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
        // An existing directory and a missing file: ERROR_FILE_NOT_FOUND on
        // Windows, ENOENT on Linux — both 2.
        let missing = std::env::temp_dir().join("os-health-probe-missing-0e1f.exe");
        assert!(!missing.exists());
        let run = SpawnProbe::with(
            move || crate::process_helpers::no_window(&missing),
            Duration::from_secs(5),
        )
        .run_once();
        assert_eq!(run.result, ProbeResult::SpawnError { code: Some(2) });

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
                error_code: Some(2),
            })
        );
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
    }
}
