//! Regression gate: the glibc arena cap
//! (`malloc_tuning::apply_arena_cap_at_startup`) is the FIRST statement of the
//! runner's `main()` in `src/main.rs`.
//!
//! ## Why this test exists
//!
//! Plan `2026-11-11-runner-glibc-arena-retention-and-whole-log-tail`, Phase 1.
//! The runner caps glibc's malloc arenas with `mallopt(M_ARENA_MAX, …)`, and
//! glibc honours that only until it fixes its arena limit, which it does the
//! first time a thread needs an arena after more than 8 exist. After that the
//! call still returns 1 and changes nothing. A cap that drifts down `main()`
//! therefore becomes no cap at all, silently: no error, no failing unit test,
//! only a runner whose RSS climbs for days again. `main()`'s first statement
//! is the one point at which the process is certainly single-threaded, and
//! `main()` keeps gaining early-startup steps at its top (the instance-root
//! check, the headless doors, the environment exports), any of which could
//! be placed above the cap.
//!
//! `arena_cap_holds.rs` shows that the cap holds when it is applied first;
//! this test keeps it first.
//!
//! ## What is checked
//!
//! A lexical check over `src/main.rs`, in the style of
//! `server_mode_flag_precedes_http_listener.rs`: `fn main() {` appears exactly
//! once; the first line after it that is neither blank nor a `//` comment
//! calls the cap; and the cap is called exactly once in the file. It is a
//! backstop, not a proof of execution order: it cannot see an attribute or a
//! macro that changes what the line does. It catches what is plausible, a step
//! added above the cap or the call moved down.

use std::path::PathBuf;

/// The runner's entry point.
const MAIN_FN: &str = "fn main() {";

/// The call that caps the arenas.
const CAP_CALL: &str = "malloc_tuning::apply_arena_cap_at_startup(";

fn main_rs() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `src-tauri/` when running inside the crate.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")
}

/// 1-based line numbers of every line `matches` accepts.
fn line_numbers(lines: &[&str], matches: impl Fn(&str) -> bool) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| matches(line))
        .map(|(index, _)| index + 1)
        .collect()
}

#[test]
fn the_arena_cap_is_mains_first_statement() {
    let path = main_rs();
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
    let lines: Vec<&str> = src.lines().collect();

    let main_lines = line_numbers(&lines, |line| line.trim_start().starts_with(MAIN_FN));
    assert_eq!(
        main_lines.len(),
        1,
        "expected exactly one `{MAIN_FN}` in {}, found it on lines {main_lines:?}",
        path.display()
    );
    let cap_lines = line_numbers(&lines, |line| line.contains(CAP_CALL));
    assert_eq!(
        cap_lines.len(),
        1,
        "expected exactly one `{CAP_CALL}` call in {}, found it on lines {cap_lines:?}. If the \
         cap moved somewhere else, move this gate with it rather than deleting it: a cap applied \
         after other threads exist is silently no cap.",
        path.display()
    );

    // `main_lines[0]` is 1-based, so it is also the index of the line after it.
    let (line_number, first_statement) = lines
        .iter()
        .enumerate()
        .skip(main_lines[0])
        .map(|(index, line)| (index + 1, line.trim()))
        .find(|(_, line)| !line.is_empty() && !line.starts_with("//"))
        .unwrap_or_else(|| panic!("`{MAIN_FN}` in {} has no body", path.display()));

    assert!(
        first_statement.contains(CAP_CALL),
        "ORDERING VIOLATION in {}: the first statement of main() (line {line_number}) is \
         `{first_statement}`, not the arena cap `{CAP_CALL})`, which is on line {}.\n\n\
         Why this matters: glibc fixes its arena limit once more than 8 arenas exist, and after \
         that mallopt(M_ARENA_MAX, …) still returns 1 and changes nothing. Anything that runs \
         before the cap may start threads, so the cap must come first.\n\n\
         Fix: move the cap call back to the top of main().",
        path.display(),
        cap_lines[0]
    );
}
