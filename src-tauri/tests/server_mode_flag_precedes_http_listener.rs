//! Regression gate: `webview_recovery::set_server_mode()` MUST run before
//! `mcp_api::start_server()` in `main.rs`'s Tauri `.setup()` closure.
//!
//! ## Why this test exists
//!
//! A runner launched with `QONTINUI_SERVER_MODE` never creates a main window,
//! so nothing can answer a UI-Bridge invoke. `perform_invoke_round_trip`
//! (`src/mcp/ui_bridge_invoke_handlers.rs`) therefore fast-fails such a call
//! with **503 `SERVER_MODE_NO_WEBVIEW`** instead of emitting an event nobody
//! receives and then reporting a 504 thirty seconds later. (Measured
//! 2026-08-29: `POST /ui-bridge/invoke/dismiss_recent_crash` → HTTP 504 after
//! 30.018s, with `/health` reporting `frontendState: "window_missing"`.)
//!
//! That gate asks exactly one question: `webview_recovery::is_server_mode()`.
//! And that accessor reads a `OnceLock<bool>` with
//!
//! ```ignore
//! *SERVER_MODE.get().unwrap_or(&false)
//! ```
//!
//! — i.e. **UNSET reads as `false`, meaning "this runner has a webview"**. The
//! default is deliberate (it protects the recovery path on a windowed runner
//! whose `main.rs` somehow skipped the setter), but it makes the whole 503 arm
//! *ordering-dependent*: if the HTTP listener could ever accept a request
//! before `set_server_mode(true)` ran, the gate would read `false` on a
//! headless runner, fall through to the emit, and the 30-second hang would be
//! back — **silently**, with no error, no log line, and every unit test still
//! green, because nothing else in the process observes the difference.
//!
//! Both call sites live in the same `.setup()` closure today, in the right
//! order. Nothing structural keeps them that way: `setup()` is a long closure
//! that gets reshuffled, and the MCP server spawn carries its own comment
//! arguing to move it EARLIER ("Spawning early lets /health bind and respond
//! during the WebView2 cold-profile init"), which is exactly the pressure that
//! would eventually push it above the flag.
//!
//! ## What is checked
//!
//! A source-order assertion over `src/main.rs`: the line calling
//! `webview_recovery::set_server_mode(` must appear before the line calling
//! `mcp_api::start_server(`. Both must appear exactly once.
//!
//! This is a lexical backstop, not a proof of execution order — it cannot see
//! a conditional, an early `return`, or a `tauri::async_runtime::spawn` that
//! reorders the two at runtime. It is precisely strong enough to catch the
//! failure mode that is actually plausible: someone moves a block while
//! editing `setup()`. If you need to restructure this, keep the invariant —
//! **set the flag before anything can serve an HTTP request** — and update
//! this test to assert whatever the new structure makes checkable.

use std::path::PathBuf;

/// Sets the process-wide server-mode flag the 503 gate reads.
const FLAG_SETTER: &str = "webview_recovery::set_server_mode(";

/// Starts the HTTP listener that serves `POST /ui-bridge/invoke/{command}`.
const LISTENER_START: &str = "mcp_api::start_server(";

fn main_rs() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `src-tauri/` when running inside the crate.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")
}

/// 1-based line numbers of every line containing `needle`.
fn lines_containing(src: &str, needle: &str) -> Vec<usize> {
    src.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(i, _)| i + 1)
        .collect()
}

#[test]
fn server_mode_flag_is_set_before_the_http_listener_starts() {
    let path = main_rs();
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));

    let setter_lines = lines_containing(&src, FLAG_SETTER);
    let listener_lines = lines_containing(&src, LISTENER_START);

    assert_eq!(
        setter_lines.len(),
        1,
        "expected exactly one `{FLAG_SETTER}` call site in {}, found {:?}. \
         If server mode is now recorded somewhere else, this gate has to move with it — \
         do not just delete it: an unset SERVER_MODE reads as `false` (\"has a webview\"), \
         which silently restores the 30-second UI-Bridge invoke hang on headless runners.",
        path.display(),
        setter_lines
    );
    assert_eq!(
        listener_lines.len(),
        1,
        "expected exactly one `{LISTENER_START}` call site in {}, found {:?}",
        path.display(),
        listener_lines
    );

    let setter_line = setter_lines[0];
    let listener_line = listener_lines[0];

    assert!(
        setter_line < listener_line,
        "ORDERING VIOLATION in {}: `{FLAG_SETTER}` is on line {setter_line} but \
         `{LISTENER_START}` is on line {listener_line}.\n\n\
         Why this matters: `webview_recovery::is_server_mode()` reads a OnceLock that \
         returns `false` when UNSET. `perform_invoke_round_trip` uses it to fast-fail \
         UI-Bridge invokes on a webview-less runner with 503 SERVER_MODE_NO_WEBVIEW. \
         If the HTTP listener can accept a request before the flag is set, that gate \
         reads `false` on a headless runner, emits `ui-bridge:invoke-request` to a window \
         that does not exist (`emit_to` on a missing label succeeds silently), and the \
         caller waits out the full 30s timeout for a 504 — the exact defect the 503 arm \
         was added to remove. Nothing else observes the flag's timing, so this regression \
         is invisible at runtime.\n\n\
         Fix: move `set_server_mode(server_mode)` back above the `mcp_api::start_server` \
         spawn in the `.setup()` closure.",
        path.display()
    );
}
