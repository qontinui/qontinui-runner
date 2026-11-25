//! The upgrade's argv doors on the runner binary, read at the top of `main`
//! BEFORE any GUI, Tauri or single-instance setup — the same posture as
//! `--capability-manifest`:
//!
//! - `qontinui-runner --provenance` prints this binary's own identity as one
//!   JSON object (`gitSha`, `gitDirty`, `buildId`, `buildProfile`) and exits 0.
//! - `qontinui-runner --upgrade-guard <record> [--after <secs>]` is the
//!   rollback guard ([`super::guard`]): no GUI, exits with its outcome.
//! - `qontinui-runner --upgrade-successor <record>` is NOT a headless door: it
//!   is a normal GUI launch by a self-managed upgrade, which must wait for its
//!   predecessor to exit before the single-instance plugin registers (else
//!   the plugin forwards this launch to the dying predecessor and exits it).
//!   [`wait_for_predecessor_if_successor`] does that wait.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A headless upgrade door named in argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeCli {
    Provenance,
    Guard {
        record: PathBuf,
        after: Option<Duration>,
    },
    /// A malformed door invocation: print and exit 2.
    Usage(String),
}

const GUARD_USAGE: &str = "usage: qontinui-runner --upgrade-guard <record.json> [--after <secs>]";

/// Match argv (argv[0] included) against the headless doors. `None` for every
/// other launch — a bare GUI launch, a `qontinui://` deep link, the `env` CLI,
/// and `--upgrade-successor` (a GUI launch).
pub fn parse(args: &[String]) -> Option<UpgradeCli> {
    match args.get(1).map(String::as_str) {
        Some("--provenance") if args.len() == 2 => Some(UpgradeCli::Provenance),
        Some("--provenance") => Some(UpgradeCli::Usage(
            "usage: qontinui-runner --provenance".into(),
        )),
        Some("--upgrade-guard") => {
            let Some(record) = args.get(2).filter(|r| !r.starts_with("--")) else {
                return Some(UpgradeCli::Usage(GUARD_USAGE.into()));
            };
            let after = match (args.get(3).map(String::as_str), args.get(4)) {
                (None, None) => None,
                (Some("--after"), Some(secs)) if args.len() == 5 => match secs.parse::<u64>() {
                    Ok(s) => Some(Duration::from_secs(s)),
                    Err(_) => return Some(UpgradeCli::Usage(GUARD_USAGE.into())),
                },
                _ => return Some(UpgradeCli::Usage(GUARD_USAGE.into())),
            };
            Some(UpgradeCli::Guard {
                record: PathBuf::from(record),
                after,
            })
        }
        _ => None,
    }
}

/// Run a headless door named in this process's argv; `Some(exit code)` when
/// one was named (the caller exits with it).
pub fn try_headless_upgrade_door() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    let code = match parse(&args)? {
        UpgradeCli::Provenance => {
            println!(
                "{}",
                qontinui_runner_lib::build_provenance::self_provenance_json()
            );
            0
        }
        UpgradeCli::Guard { record, after } => super::guard::guard_main(&record, after),
        UpgradeCli::Usage(u) => {
            println!("{u}");
            2
        }
    };
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    Some(code)
}

/// The record path of a `--upgrade-successor <record>` launch.
pub fn successor_record(args: &[String]) -> Option<PathBuf> {
    match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("--upgrade-successor"), Some(r)) => Some(PathBuf::from(r)),
        _ => None,
    }
}

/// How long a successor waits for its predecessor's exit (its handoff
/// teardown) before giving up.
pub const PREDECESSOR_WAIT: Duration = Duration::from_secs(120);

/// For a `--upgrade-successor <record>` launch: wait until the predecessor
/// named in the record has exited. Returns `Err` when it is still alive at the
/// bound (the caller exits rather than start a second runner beside it) or
/// the record is unreadable. `Ok(false)` for an ordinary launch.
pub fn wait_for_predecessor_if_successor() -> Result<bool, String> {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = successor_record(&args) else {
        return Ok(false);
    };
    let rec = super::record::read_record(&path)?;
    wait_for_exit(rec.from_pid, PREDECESSOR_WAIT, super::sys::pid_alive)?;
    Ok(true)
}

fn wait_for_exit(pid: u32, bound: Duration, alive: impl Fn(u32) -> bool) -> Result<(), String> {
    let deadline = Instant::now() + bound;
    while alive(pid) {
        if Instant::now() >= deadline {
            return Err(format!(
                "the predecessor runner (pid {pid}) is still running after {}s",
                bound.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("qontinui-runner".to_string())
            .chain(rest.iter().map(|s| (*s).to_string()))
            .collect()
    }

    #[test]
    fn only_the_upgrade_flags_divert_and_never_a_gui_launch() {
        assert_eq!(
            parse(&argv(&["--provenance"])),
            Some(UpgradeCli::Provenance)
        );
        assert_eq!(
            parse(&argv(&["--upgrade-guard", "/u/r.json"])),
            Some(UpgradeCli::Guard {
                record: "/u/r.json".into(),
                after: None
            })
        );
        assert_eq!(
            parse(&argv(&["--upgrade-guard", "/u/r.json", "--after", "180"])),
            Some(UpgradeCli::Guard {
                record: "/u/r.json".into(),
                after: Some(Duration::from_secs(180))
            })
        );
        assert!(matches!(
            parse(&argv(&["--upgrade-guard"])),
            Some(UpgradeCli::Usage(_))
        ));
        assert!(matches!(
            parse(&argv(&["--upgrade-guard", "/r", "--after", "x"])),
            Some(UpgradeCli::Usage(_))
        ));
        assert!(matches!(
            parse(&argv(&["--provenance", "--json"])),
            Some(UpgradeCli::Usage(_))
        ));
        // GUI launches fall through.
        assert_eq!(parse(&argv(&[])), None);
        assert_eq!(parse(&argv(&["qontinui://wake?intent=a"])), None);
        assert_eq!(parse(&argv(&["--upgrade-successor", "/u/r.json"])), None);
        assert_eq!(parse(&argv(&["env", "show"])), None);
        assert_eq!(parse(&argv(&["qontinui://x", "--provenance"])), None);
    }

    #[test]
    fn the_successor_launch_names_its_record() {
        assert_eq!(
            successor_record(&argv(&["--upgrade-successor", "/u/r.json"])),
            Some(PathBuf::from("/u/r.json"))
        );
        assert_eq!(successor_record(&argv(&[])), None);
    }

    #[test]
    fn the_successor_waits_for_the_predecessor_and_gives_up_at_the_bound() {
        let calls = std::cell::Cell::new(0);
        wait_for_exit(1, Duration::from_secs(5), |_| {
            calls.set(calls.get() + 1);
            calls.get() < 3
        })
        .unwrap();
        assert_eq!(calls.get(), 3);
        let err = wait_for_exit(1, Duration::from_millis(150), |_| true).unwrap_err();
        assert!(err.contains("still running"));
    }

    /// `--provenance` prints the binary's identity — the object the preflight
    /// parses from a candidate.
    #[test]
    fn provenance_door_output_is_what_the_preflight_parses() {
        let line = qontinui_runner_lib::build_provenance::self_provenance_json().to_string();
        let p = super::super::provenance::parse_binary_provenance(&line).unwrap();
        assert_eq!(p.git_sha, env!("QONTINUI_GIT_SHA"));
    }
}
