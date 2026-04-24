//! Regression guard: sync `#[tauri::command]` functions MUST NOT call
//! `block_in_place` or `block_on` (directly, via any spelling of the
//! `tokio` path).
//!
//! ## Why this test exists
//!
//! Tauri invokes sync commands on a single-threaded WebView worker that has
//! no ambient multi-thread tokio runtime. Any of the following patterns
//! panic on that thread — and the panic crosses the WebView2 FFI boundary
//! as a non-unwinding abort rather than a nice `Err`:
//!
//! ```ignore
//! tokio::task::block_in_place(|| Handle::current().block_on(...))
//! tokio::runtime::Handle::current().block_on(...)
//! handle.block_on(...)
//! ```
//!
//! The Cost Control page in the React frontend crashed the entire runner
//! process (not a React error — the Rust binary itself aborted) on
//! 2026-04-23 because `commands/meta_optimizer.rs` contained sync
//! `#[tauri::command]` functions that bottomed out in `block_in_place`.
//! Peer commands (`get_cost_dashboard`, `get_active_budget_status`) were
//! `async fn` and worked because async commands run on Tauri's tokio
//! multi-thread runtime. See the parent commit for the full fix.
//!
//! ## What this test checks
//!
//! Every `#[tauri::command]` function definition in `src/commands/*.rs`,
//! `src/mcp/*.rs`, plus the other roots that define Tauri commands is
//! scanned. For each sync `pub fn` (no `async` keyword), the function body
//! is searched for the following forbidden tokens:
//!
//! - `block_in_place`
//! - `block_on` (any caller — covers `Handle::current().block_on`,
//!   `handle.block_on`, `rt.block_on`, etc.)
//!
//! This is a **direct-call** check: if a sync command instead calls a
//! *helper* that internally uses `block_in_place`, that still crashes at
//! runtime but this test can't catch it. It's a backstop, not a proof of
//! safety. Callers are responsible for using `*_async` helpers in their
//! sync command bodies — or, preferably, making the whole command `async`.
//!
//! ## Fixing a failure
//!
//! If this test fails, convert the offending command to `pub async fn` and
//! replace the `block_on`/`block_in_place` call with a direct `.await` (or
//! a call to an `_async` helper, if one exists). See `commands/meta_optimizer.rs`
//! and `meta_optimizer/recommendations.rs` for the canonical pattern.

use std::fs;
use std::path::{Path, PathBuf};

/// Directories scanned for `#[tauri::command]` definitions.
///
/// Keep in sync with the actual call sites of `tauri::Builder::invoke_handler`.
const SCAN_ROOTS: &[&str] = &[
    "src/commands",
    "src/mcp",
    "src/error_monitor",
    "src/process_capture",
    "src/orchestration_loop",
    "src/spec_experimentation",
    "src/doctor",
];

/// Extra standalone files that define Tauri commands outside the roots above.
const SCAN_FILES: &[&str] = &[
    "src/ui_error.rs",
    "src/crash_dumps.rs",
    "src/mcp_api.rs",
    "src/ui_bridge_plugin.rs",
    "src/lib.rs",
];

fn crate_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `src-tauri/` when running inside the crate.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_rs_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let root = crate_root();

    for dir in SCAN_ROOTS {
        let path = root.join(dir);
        if path.is_dir() {
            walk(&path, &mut files);
        }
    }

    for f in SCAN_FILES {
        let path = root.join(f);
        if path.is_file() {
            files.push(path);
        }
    }

    files
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Split a `.rs` file into function-body regions that start with `#[tauri::command]`
/// and extract:
///   - whether the function is sync (no `async` keyword)
///   - the function name
///   - the function body text (naive brace-balancing)
///
/// Returns `Vec<(name, body)>` for all **sync** tauri commands in the file.
fn extract_sync_tauri_commands(source: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();

    // Find each `#[tauri::command]` attribute.
    let mut cursor = 0;
    while let Some(rel) = source[cursor..].find("#[tauri::command]") {
        let attr_start = cursor + rel;
        cursor = attr_start + "#[tauri::command]".len();

        // Walk forward through any additional attribute lines (e.g. `#[allow(...)]`).
        let mut probe = cursor;
        loop {
            // Skip whitespace / newlines.
            while probe < source.len() && (bytes[probe] as char).is_whitespace() {
                probe += 1;
            }
            // Another attribute? Skip its whole `[...]` block and continue.
            if probe < source.len() && bytes[probe] == b'#' {
                if let Some(close) = source[probe..].find(']') {
                    probe += close + 1;
                    continue;
                }
            }
            break;
        }

        // `pub fn NAME` (sync) or `pub async fn NAME` — we only care about sync.
        // Also tolerate `pub(crate)` etc., though the runner doesn't use them on
        // tauri commands today.
        let header_start = probe;
        let remaining = &source[header_start..];
        // Skip any visibility modifier like `pub` / `pub(crate)`.
        let remaining = remaining
            .strip_prefix("pub(crate)")
            .or_else(|| remaining.strip_prefix("pub(super)"))
            .or_else(|| remaining.strip_prefix("pub"))
            .unwrap_or(remaining);
        let remaining = remaining.trim_start();

        let is_async = remaining.starts_with("async fn") || remaining.starts_with("async  fn");
        if is_async {
            continue;
        }

        let after_fn = match remaining.strip_prefix("fn ") {
            Some(s) => s,
            None => continue,
        };

        // Function name is everything up to `(` or `<` (generics) or whitespace.
        let name_end = after_fn
            .find(|c: char| c == '(' || c == '<' || c.is_whitespace())
            .unwrap_or(after_fn.len());
        let name = after_fn[..name_end].to_string();

        // Find the opening brace of the body and brace-balance to the close.
        let body_start_rel = match after_fn.find('{') {
            Some(b) => b,
            None => continue,
        };
        let body_start = header_start + (remaining.len() - after_fn.len()) + body_start_rel;

        let mut depth: i32 = 0;
        let mut body_end = body_start;
        // Very naive: we don't skip strings/comments, which is fine because our
        // forbidden tokens don't legitimately appear inside Tauri command bodies
        // as string literals. If they ever do, whitelist the specific command.
        for (i, b) in source[body_start..].bytes().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = body_start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            // Couldn't balance — skip this one to avoid false positives.
            continue;
        }

        let body = source[body_start..body_end].to_string();
        out.push((name, body));
        cursor = body_end;
    }

    out
}

fn strip_line_comments(body: &str) -> String {
    body.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_sync_tauri_command_directly_calls_block_on_or_block_in_place() {
    const FORBIDDEN: &[&str] = &["block_in_place", "block_on"];

    let mut violations: Vec<String> = Vec::new();

    for file in collect_rs_files() {
        let source = match fs::read_to_string(&file) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Fast skip: if the file has no `#[tauri::command]`, nothing to check.
        if !source.contains("#[tauri::command]") {
            continue;
        }

        for (name, body) in extract_sync_tauri_commands(&source) {
            // Comments can legitimately mention these tokens — strip line comments
            // before matching so the diagnostic prose in the module doesn't trip
            // the guard. Block comments are rare in this crate and ignored.
            let body_clean = strip_line_comments(&body);
            for token in FORBIDDEN {
                if body_clean.contains(token) {
                    violations.push(format!(
                        "{}::{}  contains `{}` — convert the command to `pub async fn` and \
                         `.await` the underlying async call (or call an `_async` helper). See \
                         tests/sync_tauri_command_no_block_on.rs for the full rationale.",
                        file.strip_prefix(crate_root())
                            .unwrap_or(&file)
                            .display(),
                        name,
                        token,
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "sync `#[tauri::command]` functions must not call `block_on` or `block_in_place` \
         (they crash the runner — see test docstring). Violations:\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn extractor_recognizes_sync_vs_async_commands() {
    // Sanity check the body-extraction logic so the guard above doesn't silently
    // go blind if we refactor the parser.
    let source = r#"
        #[tauri::command]
        pub fn sync_ok() -> Result<(), String> { Ok(()) }

        #[tauri::command]
        pub async fn async_ok() -> Result<(), String> { Ok(()) }

        #[tauri::command]
        #[allow(dead_code)]
        pub fn sync_with_extra_attr() -> Result<(), String> {
            tokio::task::block_in_place(|| {});
            Ok(())
        }

        #[tauri::command]
        pub async fn async_with_block_on() -> Result<(), String> {
            // async commands are allowed to use block_on — they run on the
            // multi-thread runtime. This must NOT show up in the sync extraction.
            Handle::current().block_on(async {});
            Ok(())
        }
    "#;

    let sync = extract_sync_tauri_commands(source);
    let names: Vec<&str> = sync.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"sync_ok"), "missed sync_ok: got {:?}", names);
    assert!(
        names.contains(&"sync_with_extra_attr"),
        "missed sync_with_extra_attr: got {:?}",
        names
    );
    assert!(
        !names.contains(&"async_ok"),
        "async_ok leaked into sync list: {:?}",
        names
    );
    assert!(
        !names.contains(&"async_with_block_on"),
        "async_with_block_on leaked into sync list: {:?}",
        names
    );

    // And the forbidden-token check finds block_in_place in sync_with_extra_attr.
    let (_, body) = sync.iter().find(|(n, _)| n == "sync_with_extra_attr").unwrap();
    assert!(body.contains("block_in_place"));
}
