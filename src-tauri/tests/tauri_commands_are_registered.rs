//! Regression guard: every `#[tauri::command]` function MUST be reachable from
//! the frontend.
//!
//! ## Why this test exists
//!
//! A `#[tauri::command]` fn that is never registered with the Tauri runtime is
//! still a perfectly valid Rust fn: it compiles, it passes clippy, and every
//! unit test covering it passes. The failure surfaces only at runtime, at the
//! IPC boundary, as `Command <name> not found` — in a shipped build, in a
//! user's hands.
//!
//! This bit us three times over:
//!
//! - `github_list_repos` / `github_clone_repo` — the setup-wizard's "Clone from
//!   GitHub" picker never worked in ANY shipped build, including v1.0.4.
//! - `sign_out_full` — worse. It is the STOP-autonomy path that wipes the device
//!   JWT and the Cognito refresh token. It was unregistered, the frontend
//!   swallowed the resulting error, and the UI reported a successful sign-out
//!   while the credentials were never cleared.
//!
//! The shared root cause: each `commands/*.rs` module exposes a
//! `pub fn plugin<R: Runtime>() -> TauriPlugin<R>` carrying its own
//! `generate_handler!` list. Those `plugin()` fns are **dead scaffolding** —
//! plugin-based registration was rolled back to the central handler in commit
//! `1f1d807f` ("fix(runner): restore central invoke_handler"), because Tauri 2's
//! plugin path requires `plugin:<name>|<cmd>` invoke prefixes and the frontend
//! invokes commands bare. Adding a command to the `plugin()` list — which *looks*
//! exactly like the registration site — registers it nowhere.
//!
//! ## The invariant
//!
//! Every `#[tauri::command]` fn under the scanned roots must be registered
//! EITHER:
//!
//! 1. in `main.rs`'s central `tauri::generate_handler![...]` block (the normal
//!    path — the frontend invokes these bare, e.g. `invoke("save_settings")`), OR
//! 2. in a plugin that is **actually mounted** on the Tauri builder via
//!    `.plugin(<module>::init())` in `main.rs` (today: `ui_bridge_plugin`). Those
//!    commands are reachable under the `plugin:<name>|<cmd>` prefix, so their
//!    absence from the central handler is correct, not a bug.
//!
//! That second clause is what distinguishes a *mounted* plugin from the dead
//! `plugin()` scaffolding. A command reachable ONLY from an unmounted `plugin()`
//! fn is unreachable, and this test fails.
//!
//! ## There is deliberately NO allowlist
//!
//! An allowlist would be an escape hatch that silently re-opens the exact hole
//! this guard exists to close. If a command is genuinely not meant to be callable,
//! delete it — don't exempt it. (`save_ollama_settings`,
//! `save_openai_compatible_settings` and the template `greet` were deleted for
//! precisely this reason.)
//!
//! ## Fixing a failure
//!
//! Add the command to the central `generate_handler![...]` block in `main.rs`
//! (keep it in the block's alphabetical-within-module order). If the command is
//! dead, delete it instead.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Directories scanned for `#[tauri::command]` definitions.
///
/// Keep in sync with `sync_tauri_command_no_block_on.rs`, which scans the same
/// surface for a different property.
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

fn collect_rs_files() -> Vec<PathBuf> {
    let root = crate_root();
    let mut files = Vec::new();

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

/// Every `#[tauri::command]` fn name defined in `source`.
///
/// Matches the attribute in both its bare form and its parameterized form
/// (`#[tauri::command(rename_all = "snake_case")]`).
///
/// Two strictness rules keep this from matching PROSE about the attribute —
/// without them the scan false-positives on lines like
/// `// Tauri commands: #[tauri::command]` and
/// `if content.contains("#[tauri::command]")` (both real, in `session_recap.rs`),
/// binding them to whatever unrelated `fn` happens to come next:
///
/// 1. the attribute must START its own line (only whitespace before it), and
/// 2. it must be IMMEDIATELY followed by the fn — allowing only further
///    attributes, whitespace, and `pub` / `async` in between.
fn defined_commands(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;

    while let Some(rel) = source[cursor..].find("#[tauri::command") {
        let attr_start = cursor + rel;
        cursor = attr_start + "#[tauri::command".len();

        // (1) The attribute must be the first thing on its line — otherwise it's
        // prose (a comment or a string literal), not a real attribute.
        let line_start = source[..attr_start].rfind('\n').map_or(0, |i| i + 1);
        if !source[line_start..attr_start].trim().is_empty() {
            continue;
        }

        // Skip the rest of the attribute (handles `(rename_all = ...)`).
        let Some(attr_end_rel) = source[attr_start..].find(']') else {
            break;
        };
        let mut rest = &source[attr_start + attr_end_rel + 1..];

        // (2) Only attributes / whitespace / `pub` / `async` may sit between the
        // attribute and its `fn`. Anything else means this attribute does not
        // decorate a fn, so it is not a command definition.
        let name = loop {
            rest = rest.trim_start();

            if let Some(after) = rest.strip_prefix("fn ") {
                break after
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>();
            }

            // A further attribute, e.g. `#[allow(...)]` — skip it and retry.
            if rest.starts_with("#[") {
                let Some(end) = rest.find(']') else {
                    return out;
                };
                rest = &rest[end + 1..];
                continue;
            }

            // Visibility / asyncness — consume and retry.
            if let Some(after) = rest.strip_prefix("pub") {
                rest = after;
                // Optional `(crate)` / `(super)` / `(in path)`.
                let t = rest.trim_start();
                if t.starts_with('(') {
                    let Some(end) = t.find(')') else {
                        return out;
                    };
                    rest = &t[end + 1..];
                }
                continue;
            }
            if let Some(after) = rest.strip_prefix("async") {
                rest = after;
                continue;
            }

            // Anything else: not a fn definition.
            break String::new();
        };

        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The bare command idents inside the FIRST `generate_handler![...]` list in
/// `source`, e.g. `commands::auth::sign_out_full,` → `sign_out_full`.
fn handler_list(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(start) = source.find("generate_handler![") else {
        return out;
    };
    let body_start = start + "generate_handler![".len();

    // Balance brackets to find the end of the macro's list.
    let mut depth = 1usize;
    let mut end = body_start;
    for (i, ch) in source[body_start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = body_start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    for raw in source[body_start..end].split(',') {
        let line = raw
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("//"))
            .unwrap_or("")
            .trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(ident) = line.rsplit("::").next() {
            let ident = ident.trim();
            if !ident.is_empty() && ident.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.insert(ident.to_string());
            }
        }
    }
    out
}

/// Crate-local plugin modules ACTUALLY mounted on the Tauri builder in `main.rs`
/// via `.plugin(<module>::init())`. External `tauri_plugin_*` crates are skipped —
/// they define no commands in this crate.
///
/// A command registered in a mounted plugin's handler is reachable (under the
/// `plugin:<name>|<cmd>` prefix); one registered only in an UNMOUNTED `plugin()`
/// fn is not. That distinction is the whole point of this guard.
fn mounted_plugin_commands(main_src: &str) -> BTreeSet<String> {
    let root = crate_root();
    let mut out = BTreeSet::new();
    let mut cursor = 0;

    while let Some(rel) = main_src[cursor..].find(".plugin(") {
        let start = cursor + rel + ".plugin(".len();
        cursor = start;

        let arg: String = main_src[start..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
            .collect();

        let Some(module) = arg.split("::").next() else {
            continue;
        };
        if module.is_empty() || module.starts_with("tauri_plugin_") {
            continue;
        }

        // Resolve `foo` -> src/foo.rs or src/foo/mod.rs
        let candidates = [
            root.join("src").join(format!("{module}.rs")),
            root.join("src").join(module).join("mod.rs"),
        ];
        for path in candidates {
            if path.is_file() {
                if let Ok(src) = fs::read_to_string(&path) {
                    out.extend(handler_list(&src));
                }
                break;
            }
        }
    }
    out
}

#[test]
fn every_tauri_command_is_registered() {
    let root = crate_root();
    let main_src =
        fs::read_to_string(root.join("src/main.rs")).expect("failed to read src/main.rs");

    let central = handler_list(&main_src);
    assert!(
        central.len() > 500,
        "sanity check failed: only parsed {} commands out of main.rs's central \
         generate_handler! block — the parser is broken, not the codebase",
        central.len()
    );

    let via_mounted_plugin = mounted_plugin_commands(&main_src);

    let mut unregistered: Vec<(String, String)> = Vec::new();
    for file in collect_rs_files() {
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string()
            .replace('\\', "/");

        for name in defined_commands(&src) {
            if !central.contains(&name) && !via_mounted_plugin.contains(&name) {
                unregistered.push((name, rel.clone()));
            }
        }
    }

    assert!(
        unregistered.is_empty(),
        "\n{} `#[tauri::command]` fn(s) are defined but NEVER registered — the \
         frontend cannot invoke them, and calling one fails at runtime with \
         `Command <name> not found`:\n\n{}\n\nFix: add each to the central \
         `tauri::generate_handler![...]` block in `src-tauri/src/main.rs` \
         (alphabetical within its module). If the command is dead, DELETE it — \
         do not add an allowlist here.\n\nNOTE: adding it to a `commands/*.rs` \
         `plugin()` fn does NOT register it. Those plugins are not mounted \
         (see commit 1f1d807f); only `main.rs`'s central handler and plugins \
         actually mounted via `.plugin(...)` count.\n",
        unregistered.len(),
        unregistered
            .iter()
            .map(|(name, file)| format!("  - {name}  ({file})"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
