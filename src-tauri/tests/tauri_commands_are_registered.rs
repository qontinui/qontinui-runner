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
//! 1. in its owning module's `crate::ipc_group!(...)` list (the normal path —
//!    `ipc_registry` routes the bare name, e.g. `invoke("save_settings")`, to
//!    that module's handler), OR in `main.rs`'s central
//!    `tauri::generate_handler![...]` block while it still exists (plan
//!    `2026-10-02-split-run-app-invoke-handler` moves every module onto
//!    `ipc_group!` and then deletes it), OR
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
//! Add the command to the `crate::ipc_group!(...)` list in the module that
//! defines it (a module with no list yet gets one, plus an entry in
//! `ipc_registry::GROUPS`). If the command is dead, delete it instead.
//!
//! ## Also guarded here
//!
//! - **No name is registered twice** — in two groups, or in a group AND the
//!   central list. The router would silently pick one.
//! - **No `#[tauri::command(rename = ...)]`.** `ipc_group!` builds each group's
//!   routing names with `stringify!` on the fn ident, which equals the name
//!   Tauri registers only when there is no `rename`.

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
    // The children of the `mcp_api` module as it is split out of
    // `src/mcp_api.rs` (which stays in `SCAN_FILES` below). Absent until the
    // first move lands; a missing root is skipped, not an error.
    "src/mcp_api",
];

/// Extra standalone files that define Tauri commands outside the roots above.
const SCAN_FILES: &[&str] = &[
    "src/ui_error.rs",
    "src/crash_dumps.rs",
    "src/mcp_api.rs",
    "src/ui_bridge_plugin.rs",
    "src/lib.rs",
    "src/config_report_cmd.rs",
    "src/coord_doctor_cmd.rs",
    "src/coord_drain_state.rs",
    "src/prompt_library.rs",
    "src/repo_detection.rs",
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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

    // Strip `//` comments BEFORE splitting on commas. Splitting first is a
    // false-NEGATIVE bug, not a cosmetic one: a comma inside a comment (prose
    // commas are ordinary — "in a single shot, so the panel…") ends the chunk
    // mid-comment, so the NEXT chunk opens with the comment's own tail, which
    // no longer starts with `//`. That tail wins `find()`, fails the ident
    // check, and the real `commands::mod::name` on the following line is never
    // examined — the command reads as UNREGISTERED though it is registered
    // (observed 2026-08-20: `session_info_get`). The same shift can equally
    // MASK a genuinely unregistered command, which is this guard's whole job.
    let decommented: String = source[body_start..end]
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        );

    for raw in decommented.split(',') {
        let line = raw
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .trim();
        if line.is_empty() {
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

/// `source` with every `//` line comment removed, line structure preserved.
///
/// Line comments only, and string-unaware: a `//` inside a string literal
/// (`"https://…"`) also cuts the line. Harmless for what this file scans —
/// `ipc_group!` lists, `GROUPS` entries and `#[tauri::command(...)]`
/// attributes carry no string literals containing `//`.
fn decomment(source: &str) -> String {
    source
        .lines()
        .map(|l| l.split_once("//").map_or(l, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bodies of every `ipc_group!` invocation in `source`, whichever of the
/// three macro delimiters it uses (`(…)`, `[…]`, `{…}`). Comments are stripped
/// first, so prose and doc examples mentioning the macro never contribute.
fn ipc_group_bodies(source: &str) -> Vec<String> {
    let decommented = decomment(source);
    let mut out = Vec::new();
    let mut rest = decommented.as_str();
    while let Some((_, after)) = rest.split_once("ipc_group!") {
        let after = after.trim_start();
        let mut chars = after.chars();
        let close = match chars.next() {
            Some('(') => ')',
            Some('[') => ']',
            Some('{') => '}',
            // `macro_rules! ipc_group` itself, or a non-invocation mention.
            _ => {
                rest = after;
                continue;
            }
        };
        let Some((body, more)) = chars.as_str().split_once(close) else {
            break;
        };
        out.push(body.to_string());
        rest = more;
    }
    out
}

/// Every bare command ident listed in any `ipc_group!` invocation in `source`,
/// e.g. `crate::ipc_group!(a11y_capture, a11y_click);`.
fn ipc_group_lists(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for body in ipc_group_bodies(source) {
        for raw in body.split(',') {
            let ident = raw.trim();
            if !ident.is_empty() && ident.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.push(ident.to_string());
            }
        }
    }
    out
}

/// `src/commands/foo.rs` -> `commands::foo`; `src/foo/mod.rs` -> `foo`.
fn module_path_of(rel_from_src: &str) -> String {
    let p = rel_from_src.trim_end_matches(".rs");
    let p = p.strip_suffix("/mod").unwrap_or(p);
    p.replace('/', "::")
}

/// Module paths named in `ipc_registry::GROUPS`, read from its
/// `crate::<path>::IPC_GROUP` entries in `src/ipc_registry.rs`.
fn groups_registry_modules() -> BTreeSet<String> {
    let src = fs::read_to_string(crate_root().join("src/ipc_registry.rs"))
        .expect("failed to read src/ipc_registry.rs");
    let decommented = decomment(&src);
    let pieces: Vec<&str> = decommented.split("::IPC_GROUP").collect();
    let mut out = BTreeSet::new();
    // Every piece but the last ends where a `::IPC_GROUP` began; its trailing
    // path characters are the module path.
    for piece in pieces.iter().take(pieces.len().saturating_sub(1)) {
        let mut path: Vec<char> = piece
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        path.reverse();
        let path: String = path.into_iter().collect();
        if let Some(module) = path.strip_prefix("crate::") {
            out.insert(module.to_string());
        }
    }
    out
}

/// Every `.rs` file under `src/` — `ipc_group!` lists are collected crate-wide,
/// not only from the scan roots, so a group can never be invisible here.
fn all_src_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(&crate_root().join("src"), &mut files);
    files
}

/// Crate-local plugin modules ACTUALLY mounted on the Tauri builder in `main.rs`
/// via `.plugin(<module>::init())`. External `tauri_plugin_*` crates are skipped —
/// they define no commands in this crate.
///
/// A command registered in a mounted plugin's handler is reachable (under the
/// `plugin:<name>|<cmd>` prefix); one registered only in an UNMOUNTED `plugin()`
/// fn is not. That distinction is the whole point of this guard.
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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

    // name -> every file whose `ipc_group!` lists it
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    // module path of every file that invokes `ipc_group!`
    let mut group_modules: BTreeSet<String> = BTreeSet::new();
    for file in all_src_files() {
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(crate_root())
            .unwrap_or(&file)
            .display()
            .to_string()
            .replace('\\', "/");
        if !ipc_group_bodies(&src).is_empty() {
            group_modules.insert(module_path_of(rel.trim_start_matches("src/")));
        }
        for name in ipc_group_lists(&src) {
            grouped.entry(name).or_default().push(rel.clone());
        }
    }

    // A module whose `ipc_group!` is missing from `GROUPS` compiles cleanly
    // (dead code is allowed crate-wide) and then answers every one of its
    // commands with "Command not found" — the 1f1d807f failure. The reverse
    // (a `GROUPS` entry with no `ipc_group!`) does not compile.
    let in_registry = groups_registry_modules();
    let unrouted: Vec<&String> = group_modules.difference(&in_registry).collect();
    assert!(
        unrouted.is_empty(),
        "\nmodule(s) invoke `ipc_group!` but have no entry in `ipc_registry::GROUPS`, \
         so the router never reaches their commands (\"Command not found\" at \
         runtime): {unrouted:?}\n\nFix: add `crate::<module>::IPC_GROUP` to `GROUPS` \
         in src-tauri/src/ipc_registry.rs.\n"
    );
    assert!(
        central.len() + grouped.len() > 500,
        "sanity check failed: only parsed {} central + {} ipc_group! commands — \
         the parser is broken, not the codebase",
        central.len(),
        grouped.len()
    );

    let double_registered: Vec<String> = grouped
        .iter()
        .filter(|(name, files)| files.len() > 1 || central.contains(*name))
        .map(|(name, files)| {
            let central_note = if central.contains(name) {
                " + main.rs central list"
            } else {
                ""
            };
            format!("  - {name}  ({}{central_note})", files.join(", "))
        })
        .collect();
    assert!(
        double_registered.is_empty(),
        "\n{} command name(s) are registered more than once — the IPC router \
         would silently pick one registration:\n\n{}\n\nKeep exactly one: the \
         `ipc_group!` list in the module that defines the command.\n",
        double_registered.len(),
        double_registered.join("\n"),
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
            if !central.contains(&name)
                && !grouped.contains_key(&name)
                && !via_mounted_plugin.contains(&name)
            {
                unregistered.push((name, rel.clone()));
            }
        }
    }

    assert!(
        unregistered.is_empty(),
        "\n{} `#[tauri::command]` fn(s) are defined but NEVER registered — the \
         frontend cannot invoke them, and calling one fails at runtime with \
         `Command <name> not found`:\n\n{}\n\nFix: add each to the \
         `crate::ipc_group!(...)` list in the module that defines it (see \
         `src-tauri/src/ipc_registry.rs`). If the command is dead, DELETE it — \
         do not add an allowlist here.\n\nNOTE: a Tauri *plugin* does NOT \
         register a bare command — plugin commands are reachable only as \
         `plugin:<name>|<cmd>` (see commit 1f1d807f); only `ipc_group!` lists, \
         the central handler and plugins mounted via `.plugin(...)` count.\n",
        unregistered.len(),
        unregistered
            .iter()
            .map(|(name, file)| format!("  - {name}  ({file})"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// `ipc_group!` routes by `stringify!(<fn>)`, which is the registered name only
/// when the command carries no `rename`. A renamed command would route
/// nowhere, so the attribute is refused outright — in both the
/// `#[tauri::command(...)]` and the imported `#[command(...)]` spelling, and
/// across a multi-line attribute.
#[test]
fn no_tauri_command_is_renamed() {
    let mut renamed = Vec::new();
    for file in all_src_files() {
        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };
        let src = decomment(&src);
        for marker in ["#[tauri::command(", "#[command("] {
            for (start, _) in src.match_indices(marker) {
                let tail = src.get(start..).unwrap_or_default();
                let attr = tail.split_once(")]").map_or(tail, |(attr, _)| attr);
                if attr.replace("rename_all", "").contains("rename") {
                    let line = src.get(..start).map_or(0, |s| s.matches('\n').count()) + 1;
                    renamed.push(format!("  - {}:{line}", file.display()));
                }
            }
        }
    }
    assert!(
        renamed.is_empty(),
        "\n`#[tauri::command(rename = ...)]` is not supported — `ipc_group!` routes \
         by the fn name. Rename the fn instead:\n\n{}\n",
        renamed.join("\n"),
    );
}
