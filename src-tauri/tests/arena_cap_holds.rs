//! Gate: with the arena cap applied first, 32 live allocating threads leave
//! `malloc_info` at no more than `ARENA_CAP` arenas.
//!
//! Plan `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail`, Phase 1.
//! The runner caps glibc's malloc arenas as `main()`'s first statement
//! (`malloc_tuning::apply_arena_cap_at_startup`). This test applies the cap
//! the same way, starts 32 threads that each allocate and stay alive, and
//! counts the arenas in `malloc_info`'s output with the module's own reader.
//!
//! # Why one test, measured in child processes
//!
//! glibc fixes its arena limit the first time a thread needs an arena after
//! more than `arena_test` (8 on 64-bit) arenas exist, and ignores
//! `M_ARENA_MAX` from then on. So the measurement is only sound in a process
//! where few threads have touched malloc before the cap. This binary holds ONE
//! test, so a measuring process has libtest's main thread and one test thread,
//! and so at most 2 arenas, when the cap goes on.
//!
//! The test re-runs itself as two child processes and measures there, with
//! glibc's arena settings removed from their environment. glibc reads them at
//! process start. An inherited `MALLOC_ARENA_MAX` or `GLIBC_TUNABLES` would
//! make the runner leave the cap to the environment, so the test would measure
//! the caller's cap rather than `ARENA_CAP`. `MALLOC_ARENA_TEST` is removed
//! too, as hygiene rather than for a known failure, so that no inherited glibc
//! arena setting is in play in either arm: with two threads before the cap,
//! even `MALLOC_ARENA_TEST=1` leaves the cap holding at 2 on glibc 2.41.
//!
//! - **capped** applies the cap as `main()` does, and must see 1 to
//!   `ARENA_CAP` arenas.
//! - **control** applies nothing, and must see MORE than `ARENA_CAP`. That is
//!   what makes a capped pass mean something: without it, a reader that
//!   undercounts, or threads that never spread across arenas, would pass the
//!   capped arm too.

#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::collections::HashMap;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

use qontinui_runner_lib::malloc_tuning;

/// This binary's one test; a child runs it again by exact name.
const TEST_NAME: &str = "arena_cap_holds_under_32_live_allocating_threads";
/// Set on a child process to the arm it measures: `capped` or `control`.
const CHILD_ARM_ENV: &str = "ARENA_CAP_HOLDS_ARM";
/// Marks the one line on which a child reports its measurement.
const REPORT_MARKER: &str = "ARENA_CAP_HOLDS_REPORT";
/// glibc's arena settings, which a child must not inherit (see the module doc).
const GLIBC_ARENA_ENV: [&str; 3] = ["MALLOC_ARENA_MAX", "MALLOC_ARENA_TEST", "GLIBC_TUNABLES"];
/// Threads that allocate and stay alive while the arenas are counted.
const THREADS: usize = 32;

#[test]
fn arena_cap_holds_under_32_live_allocating_threads() {
    if let Ok(arm) = std::env::var(CHILD_ARM_ENV) {
        measure(&arm);
        return;
    }

    let cap = malloc_tuning::ARENA_CAP;
    let capped = report(&run_child("capped"), "capped");
    let control = report(&run_child("control"), "control");
    println!("capped: {capped:?}\ncontrol: {control:?}");

    assert_eq!(
        capped.get("source").map(String::as_str),
        Some("compiled"),
        "the capped child ran with {GLIBC_ARENA_ENV:?} removed from its environment, so the \
         runner must have applied its own cap; it reported {capped:?}"
    );
    let capped_count = arena_count(&capped, "capped");
    let control_count = arena_count(&control, "control");

    assert!(
        control_count > cap,
        "CONTROL ARM: with no cap, {THREADS} live allocating threads spread over only \
         {control_count} arenas, which is not more than the cap ({cap}). On this box the test \
         cannot tell a held cap from no cap, so a capped pass would mean nothing."
    );
    assert!(
        (1..=cap).contains(&capped_count),
        "CAP NOT HELD: with the cap applied first, {THREADS} live allocating threads left \
         malloc_info at {capped_count} arenas against a cap of {cap} (the uncapped control arm \
         reached {control_count}). glibc ignores M_ARENA_MAX once it has fixed its arena limit, \
         which it does after more than 8 arenas exist."
    );
}

/// A child's whole job: apply the cap (capped arm only), count the arenas
/// while 32 allocating threads are alive, and print one report line.
fn measure(arm: &str) {
    let mut line = format!("\n{REPORT_MARKER} arm={arm}");
    match arm {
        "capped" => {
            malloc_tuning::apply_arena_cap_at_startup();
            let attempt =
                malloc_tuning::startup_attempt().expect("the cap call records its attempt");
            line.push_str(&format!(
                " source={} requested={} mallopt_rc={:?}",
                attempt.decision.source.as_str(),
                attempt.decision.cap,
                attempt.mallopt_rc
            ));
        }
        "control" => {}
        other => panic!("unknown arm {other:?}"),
    }
    let count = arena_count_with_live_threads();
    line.push_str(&format!(
        " arena_count={}",
        count.map_or_else(|| "unknown".to_string(), |count| count.to_string())
    ));
    println!("{line}");
}

/// Count the arenas while [`THREADS`] threads hold allocations and stay alive.
/// A thread is bound to an arena by its first malloc, so every thread has
/// chosen one by the time the count is taken.
fn arena_count_with_live_threads() -> Option<u64> {
    let allocated = Arc::new(Barrier::new(THREADS + 1));
    let counted = Arc::new(Barrier::new(THREADS + 1));
    let workers: Vec<_> = (0..THREADS)
        .map(|index| {
            let (allocated, counted) = (Arc::clone(&allocated), Arc::clone(&counted));
            std::thread::spawn(move || {
                let held: Vec<Vec<u8>> = (0..64).map(|_| vec![index as u8; 4096]).collect();
                allocated.wait();
                counted.wait();
                drop(std::hint::black_box(held));
            })
        })
        .collect();
    allocated.wait();
    let count = malloc_tuning::sample().arena_count;
    counted.wait();
    for worker in workers {
        worker.join().expect("an allocating thread panicked");
    }
    count
}

/// Run this binary's one test again as a child measuring `arm`, with glibc's
/// arena settings removed from its environment.
fn run_child(arm: &str) -> Output {
    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut child = Command::new(&exe);
    child
        .args([TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ARM_ENV, arm);
    for name in GLIBC_ARENA_ENV {
        child.env_remove(name);
    }
    child
        .output()
        .unwrap_or_else(|e| panic!("could not run the {arm} child ({}): {e}", exe.display()))
}

/// The `key=value` fields of a child's report line. Fails when the child
/// failed or printed no report.
fn report(output: &Output, arm: &str) -> HashMap<String, String> {
    assert!(
        output.status.success(),
        "the {arm} child failed ({}):\n{}",
        output.status,
        indented(output)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // libtest may print `test <name> ... ` on the same line before the report,
    // so the marker is found anywhere in a line, not only at its start.
    let fields = stdout
        .lines()
        .find_map(|line| line.split_once(REPORT_MARKER).map(|(_, fields)| fields))
        .unwrap_or_else(|| {
            panic!(
                "the {arm} child printed no {REPORT_MARKER} line:\n{}",
                indented(output)
            )
        });
    fields
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// A child's arena count. An `unknown` count fails the test: the cap is then
/// UNKNOWN, not held.
fn arena_count(report: &HashMap<String, String>, arm: &str) -> u64 {
    let raw = report
        .get("arena_count")
        .unwrap_or_else(|| panic!("the {arm} child reported no arena_count: {report:?}"));
    raw.parse().unwrap_or_else(|_| {
        panic!(
            "the {arm} child's arena count is {raw:?}: malloc_info could not be read, so whether \
             the cap holds is UNKNOWN"
        )
    })
}

/// A child's output, indented so that none of its lines reads as one of this
/// binary's own libtest lines to a log scraper.
fn indented(output: &Output) -> String {
    let mut text = String::new();
    for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        text.push_str(&format!("  [{stream}]\n"));
        for line in String::from_utf8_lossy(bytes).lines() {
            text.push_str("  | ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}
