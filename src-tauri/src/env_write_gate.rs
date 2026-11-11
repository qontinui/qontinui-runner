//! The test-binary gate on ENV-RESOLVED settings writes: a thread that holds no
//! `env_lock` may not write a settings file whose path it resolved from the
//! process environment.
//!
//! # Why
//!
//! `ambient::test_support::IsolatedAmbient` takes the one process-wide
//! `env_lock` for its whole life and points `QONTINUI_CONFIG_DIR`, `HOME` and
//! three more keys at a fresh tempdir. Fixtures exclude each other; a thread
//! that holds no `env_lock` is excluded by nothing and resolves whatever the
//! process environment names at that instant — another test's fixture. When
//! such a thread reaches a settings loader, `settings::load_settings_full`
//! finds no `settings.json` there, loads `FreshInstall`, mints a
//! `local_user_id` and PERSISTS a whole default document into that other
//! test's directory. That was the `config_report_live_command_writes_nothing_it_reports_on`
//! red (coord finding `47c0445a`; 8780 bytes appearing from nothing), and a
//! measured run showed 32 such writes from 26 threads, 29 of them inside a live
//! fixture. Plan
//! `2026-11-11-unguarded-test-threads-write-settings-json-into-a-live-fixtures-config-dir`.
//!
//! # The predicate
//!
//! **Allowed iff THIS thread holds `env_lock`** (`env_lock_depth() > 0`).
//! Fixtures, plain `env_lock()` holders and every `hierarchy_lock` child all
//! count. It reads only this thread's state, so the same writer gets the same
//! answer on every run (no flake moves to the writer), it acquires nothing (no
//! AB/BA against `AI_SETTINGS_WRITE_LOCK`), and while this thread holds the lock
//! no fixture can change the environment underneath it. It deliberately does
//! NOT ask whether some fixture is live — the read canary
//! (`ambient::canary`, arm 3) asks that, correctly for a read and wrongly for a
//! write: a dir no fixture owns at this instant can still be a plain
//! `env_lock` holder's dir, or a fixture's in its teardown window.
//!
//! A thread a guarded test spawns (a `std::thread`, a tokio worker, a blocking
//! pool thread) holds nothing and is refused — it cannot take the lock its
//! parent holds without blocking forever, which is why the remedy names the
//! parent's thread.
//!
//! # Arming
//!
//! Only a cargo-built test binary evaluates the predicate, by the read
//! canary's proven arming: compiled under `any(test, debug_assertions)` and
//! gated at runtime on `ambient::test_support::canary_armed()`, because the
//! runner BIN's test binary links this lib compiled without `cfg(test)`. A
//! shipped build compiles the `Ok(())` twin; a dev runner is not in a `deps/`
//! directory and is never armed. So production behaviour is unchanged.
//!
//! # The doors
//!
//! Callers are the writers that resolve their target from the environment:
//! `settings::get_settings_path` (every whole-document save),
//! `settings::load_settings_full`'s migration persist,
//! `settings::update_settings`, `profiles::promote_tier_to_account`, the
//! env-resolved `claude_accounts` writers and `backup::restore_backup`.
//! Path-explicit writers are not gated: a caller holding a path is not reading
//! the shared environment, so it cannot land in another test's directory.
//!
//! # A refusal
//!
//! Counted (process-wide and per thread, so tests assert on state rather than
//! stderr), then a `panic!` under `test_support::strict_canary()`, else ONE
//! line on stderr per distinct `(thread, door, first caller frame)` — written
//! with `Write::write_all` on `std::io::stderr()`, which libtest's per-test
//! capture does not intercept (it intercepts the `print!` family only), so the
//! line reaches the suite log even from a passing test — and an `Err` carrying
//! the same sentence. The caller frame is in the key because many runtime
//! workers share one thread name (`tokio-rt-worker`); without it, the first
//! worker's line would swallow every other worker's.

use std::path::Path;

/// The plan every refusal names.
pub const PLAN: &str =
    "2026-11-11-unguarded-test-threads-write-settings-json-into-a-live-fixtures-config-dir";

/// The prefix of the stderr line a refusal writes — what a suite log is
/// grepped for to inventory the refusals of one run.
pub const LINE_PREFIX: &str = "[env-write-gate]";

/// May THIS thread write `target`, a settings file `door` resolved from the
/// process environment?
///
/// `Ok(())` outside a test binary, and inside one when this thread holds
/// `env_lock`. Otherwise the write is refused: see the module docs for what a
/// refusal records, prints and returns. `target` is what the door would have
/// written, from a NON-creating resolver, so a refusal creates nothing.
#[cfg(any(test, debug_assertions))]
pub fn check(door: &'static str, target: Option<&Path>) -> Result<(), String> {
    use crate::ambient::test_support::{canary_armed, env_lock_depth};
    if !canary_armed() || env_lock_depth() > 0 {
        return Ok(());
    }
    Err(refuse(door, target))
}

/// A shipped build carries no gate: `debug_assertions` is off and the
/// `test_support` state the predicate reads is not compiled.
#[cfg(not(any(test, debug_assertions)))]
#[inline(always)]
pub fn check(_door: &'static str, _target: Option<&Path>) -> Result<(), String> {
    Ok(())
}

#[cfg(any(test, debug_assertions))]
pub use armed::{refusals, refusals_on_this_thread, reported_lines};

#[cfg(any(test, debug_assertions))]
use armed::refuse;

#[cfg(any(test, debug_assertions))]
mod armed {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Refusals in this process, on any thread.
    static REFUSALS: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        /// Refusals on THIS thread — what a test reads, immune to every other
        /// test running in parallel.
        static REFUSALS_HERE: Cell<u64> = const { Cell::new(0) };
    }

    /// The `(thread, door, caller frame)` keys a line has been written for.
    static REPORTED: Mutex<BTreeSet<(String, &'static str, String)>> = Mutex::new(BTreeSet::new());

    /// What a frame list falls back to when no frame lies outside the doors.
    const NO_CALLER_FRAME: &str = "<no frame outside the settings doors>";

    /// The modules whose frames are the door's own code rather than the caller
    /// that reached it: the gate, and every module a gated door lives in or
    /// reaches a door through. A frame in a test module beneath one of them
    /// (`settings::tests::…`) is a caller, not door code.
    const DOOR_MODULES: &[&str] = &[
        "qontinui_runner_lib::env_write_gate::",
        "qontinui_runner_lib::profiles::",
        "qontinui_runner::settings::",
        "qontinui_runner::config_facade::",
        "qontinui_runner::claude_accounts::",
        "qontinui_runner::backup::",
    ];

    /// The language runtime and the unwinder: never the caller.
    const RUNTIME_PREFIXES: &[&str] = &["std::", "core::", "alloc::", "backtrace::"];

    /// How many refusals this process has made, on every thread.
    pub fn refusals() -> u64 {
        REFUSALS.load(Ordering::SeqCst)
    }

    /// How many refusals THIS thread has made.
    pub fn refusals_on_this_thread() -> u64 {
        REFUSALS_HERE.try_with(Cell::get).unwrap_or(0)
    }

    /// How many distinct refusal LINES this process has written for `thread`
    /// at `door` — one per caller frame, however often that frame was refused.
    pub fn reported_lines(thread: &str, door: &str) -> usize {
        REPORTED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(t, d, _)| t == thread && *d == door)
            .count()
    }

    /// Record, report and phrase one refusal. Returns the sentence the door
    /// hands back as its `Err`.
    pub(super) fn refuse(door: &'static str, target: Option<&Path>) -> String {
        REFUSALS.fetch_add(1, Ordering::SeqCst);
        let _ = REFUSALS_HERE.try_with(|c| c.set(c.get() + 1));

        let thread = thread_label();
        let caller = first_caller_frame();
        let refusal = sentence(door, target, &thread, &caller);
        if crate::ambient::test_support::thread_is_strict() {
            panic!("{refusal}");
        }
        report_once(&thread, door, &caller, &refusal);
        refusal
    }

    fn thread_label() -> String {
        let current = std::thread::current();
        match current.name() {
            Some(name) => name.to_string(),
            None => format!("<unnamed {:?}>", current.id()),
        }
    }

    pub(super) fn sentence(
        door: &str,
        target: Option<&Path>,
        thread: &str,
        caller: &str,
    ) -> String {
        let target = target.map_or_else(
            || "<a path that did not resolve>".to_string(),
            |p| p.display().to_string(),
        );
        format!(
            "env-resolved settings write refused in a test binary: thread `{thread}` holds no \
             env_lock, so `{door}` (reached from `{caller}`) would write `{target}` in a \
             directory another test may own — issue the write from the thread that holds \
             `isolated_ambient()` (a thread spawned by a guarded test cannot take its lock; it \
             would block forever), hold `isolated_ambient()` for the whole test if none is \
             held, or pass an explicit path (plan {plan})",
            plan = super::PLAN,
        )
    }

    /// Write the refusal line unless this `(thread, door, caller)` already has
    /// one. Direct `write_all`, NOT `eprintln!`: libtest captures the `print!`
    /// family per test, and a captured line from a passing test is discarded.
    fn report_once(thread: &str, door: &'static str, caller: &str, sentence: &str) {
        let first = REPORTED.lock().unwrap_or_else(|p| p.into_inner()).insert((
            thread.to_string(),
            door,
            caller.to_string(),
        ));
        if first {
            use std::io::Write as _;
            let line = format!("{} {sentence}\n", super::LINE_PREFIX);
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    /// The first frame on this thread's stack that is neither the runtime nor
    /// door code: the caller that reached the door. Test builds carry symbol
    /// tables, so a capture resolves names without debuginfo.
    fn first_caller_frame() -> String {
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        first_frame_outside_the_doors(&trace)
    }

    /// [`first_caller_frame`] over a rendered backtrace — pure, so the frame
    /// rule is testable against a fixed trace.
    pub(super) fn first_frame_outside_the_doors(trace: &str) -> String {
        trace
            .lines()
            .filter_map(frame_symbol)
            .find(|symbol| !is_runtime_frame(symbol) && !is_door_frame(symbol))
            .map_or_else(|| NO_CALLER_FRAME.to_string(), str::to_string)
    }

    /// `"  12: a::b::c"` → `Some("a::b::c")`. A file/line continuation line
    /// has no numeric index and is skipped.
    fn frame_symbol(line: &str) -> Option<&str> {
        let (index, symbol) = line.trim_start().split_once(": ")?;
        (!index.is_empty() && index.bytes().all(|b| b.is_ascii_digit())).then_some(symbol.trim())
    }

    /// `<…>` opens a trait-impl frame (`<a::B as c::D>::f`); its leading path
    /// decides.
    fn unqualified(symbol: &str) -> &str {
        symbol.trim_start_matches('<')
    }

    fn is_runtime_frame(symbol: &str) -> bool {
        let symbol = unqualified(symbol);
        symbol.starts_with("__rust")
            || symbol == "<unknown>"
            || RUNTIME_PREFIXES.iter().any(|p| symbol.starts_with(p))
    }

    fn is_door_frame(symbol: &str) -> bool {
        let symbol = unqualified(symbol);
        DOOR_MODULES.iter().any(|module| {
            symbol
                .strip_prefix(module)
                .is_some_and(|rest| !names_a_test_module(rest))
        })
    }

    fn names_a_test_module(rest: &str) -> bool {
        rest.split("::")
            .any(|segment| segment == "tests" || segment.ends_with("_tests"))
    }
}

#[cfg(test)]
mod tests {
    use super::armed::{first_frame_outside_the_doors, sentence};
    use super::*;
    use std::path::PathBuf;

    const DOOR: &str = "env_write_gate::tests::door";

    fn target() -> PathBuf {
        PathBuf::from("/fixture/settings.json")
    }

    #[test]
    fn a_thread_holding_env_lock_is_allowed_and_counts_nothing() {
        let _env = crate::test_env::env_lock();
        let before = refusals_on_this_thread();
        assert_eq!(check(DOOR, Some(&target())), Ok(()));
        assert_eq!(refusals_on_this_thread(), before);
    }

    #[test]
    fn a_thread_holding_no_env_lock_is_refused_and_counted() {
        // A fresh thread starts at depth 0 whatever the test thread holds.
        let (verdict, here, process_after) = std::thread::spawn(|| {
            let verdict = check(DOOR, Some(&target()));
            (verdict, refusals_on_this_thread(), refusals())
        })
        .join()
        .expect("the probe thread must not panic");
        let refused = verdict.expect_err("an unguarded write must be refused");
        assert!(refused.contains("holds no env_lock"), "{refused}");
        assert_eq!(here, 1, "the refusal is counted on the refused thread");
        assert!(process_after >= 1, "and in the process-wide count");
    }

    #[test]
    fn the_sentence_names_door_path_thread_caller_and_plan() {
        let name = "env-write-gate-sentence-probe";
        let refused = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(|| check(DOOR, Some(&target())))
            .expect("spawn the probe thread")
            .join()
            .expect("the probe thread must not panic")
            .expect_err("an unguarded write must be refused");
        for needle in [
            format!("thread `{name}` holds no env_lock"),
            format!("`{DOOR}`"),
            format!("would write `{}`", target().display()),
            "reached from `".to_string(),
            PLAN.to_string(),
        ] {
            assert!(
                refused.contains(&needle),
                "missing {needle:?} in: {refused}"
            );
        }
    }

    #[test]
    fn the_strict_arm_panics_with_the_sentence() {
        let payload = std::thread::spawn(|| {
            let _strict = crate::ambient::test_support::strict_canary();
            let _ = check(DOOR, Some(&target()));
        })
        .join()
        .expect_err("a strict unguarded write must panic");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .expect("the panic carries the formatted sentence");
        assert!(message.contains("holds no env_lock"), "{message}");
        assert!(message.contains(DOOR), "{message}");
    }

    #[test]
    fn a_refusal_writes_one_line_per_thread_door_and_caller() {
        let name = "env-write-gate-print-once-probe-lib";
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(|| {
                for _ in 0..3 {
                    let _ = check(DOOR, Some(&target()));
                }
                assert_eq!(refusals_on_this_thread(), 3);
            })
            .expect("spawn the probe thread")
            .join()
            .expect("the probe thread must not panic");
        assert_eq!(reported_lines(name, DOOR), 1);
    }

    /// The caller frame skips the runtime and the door modules, keeps a test
    /// module beneath a door module, and reads a trait-impl frame by its
    /// leading path.
    #[test]
    fn the_caller_frame_is_the_first_outside_the_runtime_and_the_doors() {
        let trace = "\
   0: std::backtrace_rs::backtrace::libunwind::trace
   1: std::backtrace::Backtrace::create
   2: qontinui_runner_lib::env_write_gate::armed::refuse
   3: qontinui_runner_lib::env_write_gate::check
   4: qontinui_runner::settings::load_settings_full
   5: <qontinui_runner::settings::PathSettings as qontinui_runner::config_facade::SettingsField>::get_from
   6: qontinui_runner::config_facade::get_setting
   7: qontinui_runner::workspace_paths::workspace_root
   8: qontinui_runner::coord_mcp::tests::a_test";
        assert_eq!(
            first_frame_outside_the_doors(trace),
            "qontinui_runner::workspace_paths::workspace_root"
        );

        let from_a_door_module_test = "\
   0: qontinui_runner_lib::env_write_gate::check
   1: qontinui_runner::settings::update_settings
   2: qontinui_runner::settings::gate_tests::writes_from_a_worker::{{closure}}
   3: core::ops::function::FnOnce::call_once";
        assert_eq!(
            first_frame_outside_the_doors(from_a_door_module_test),
            "qontinui_runner::settings::gate_tests::writes_from_a_worker::{{closure}}"
        );

        assert_eq!(
            first_frame_outside_the_doors("   0: std::rt::lang_start\n   1: <unknown>"),
            "<no frame outside the settings doors>"
        );
    }

    /// A live capture names the probe that reached the gate, not the gate.
    #[test]
    fn a_live_refusal_names_the_function_that_reached_the_gate() {
        #[inline(never)]
        fn gate_caller_probe() -> Result<(), String> {
            check(DOOR, Some(&target()))
        }
        let refused = std::thread::spawn(gate_caller_probe)
            .join()
            .expect("the probe thread must not panic")
            .expect_err("an unguarded write must be refused");
        assert!(
            refused.contains("reached from `qontinui_runner_lib::env_write_gate::tests::"),
            "{refused}"
        );
    }

    #[test]
    fn an_unresolved_target_is_named_as_such() {
        let text = sentence(DOOR, None, "t", "c");
        assert!(
            text.contains("would write `<a path that did not resolve>`"),
            "{text}"
        );
    }
}
