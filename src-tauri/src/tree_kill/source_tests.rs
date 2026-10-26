//! Source pins for plan `2026-10-25-runner-kill-paths-never-kill-anything-off-windows`,
//! Phase 2 (shape: `terminal/graceful_exit.rs` `this_module_contains_no_kill_path`).
//!
//! (a) The eight kill sites' files keep no "does nothing off Windows" arm.
//! (b) Nothing pushes into an AI pid tracker except [`super::track_ai_pid`],
//!     so every tracked pid has a recorded spawn identity.

/// The files holding the eight kill paths, read at compile time.
const KILL_SITE_FILES: [(&str, &str); 6] = [
    ("mcp/ai_session.rs", include_str!("../mcp/ai_session.rs")),
    ("mcp/task_runs.rs", include_str!("../mcp/task_runs.rs")),
    (
        "graphql/mutation.rs",
        include_str!("../graphql/mutation.rs"),
    ),
    (
        "mcp/headless_browser.rs",
        include_str!("../mcp/headless_browser.rs"),
    ),
    ("doctor/commands.rs", include_str!("../doctor/commands.rs")),
    (
        "ai_provider/pi_cli.rs",
        include_str!("../ai_provider/pi_cli.rs"),
    ),
];

fn without_comment_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `source` without its module-level test modules: each column-0
/// `#[cfg(test)]` (or `#[cfg(all(test, …))]`) followed by a `mod` item is cut,
/// through that module's closing column-0 `}` (rustfmt'd code). Everything
/// else — production code AFTER a test module, and method-level
/// `#[cfg(test)]` items — is kept (review r1, finding 2: cutting at the FIRST
/// `#[cfg(test)]` dropped both of `mcp/ai_session.rs`'s kill sites).
fn production_code(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let is_test_cfg = |l: &str| l == "#[cfg(test)]" || l.starts_with("#[cfg(all(test");
    let is_mod = |l: &str| {
        ["mod ", "pub mod ", "pub(crate) mod ", "pub(super) mod "]
            .iter()
            .any(|p| l.starts_with(p))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if is_test_cfg(lines[i]) {
            let mut j = i + 1;
            while j < lines.len() && (lines[j].starts_with("#[") || lines[j].starts_with("///")) {
                j += 1;
            }
            if j < lines.len() && is_mod(lines[j]) {
                if lines[j].trim_end().ends_with(';') {
                    i = j + 1;
                    continue;
                }
                let mut k = j + 1;
                while k < lines.len() && lines[k] != "}" {
                    k += 1;
                }
                i = k + 1;
                continue;
            }
        }
        out.push(lines[i]);
        i += 1;
    }
    out.join(
        "
",
    )
}

#[test]
fn production_code_cuts_test_modules_only() {
    let src = "fn a() {}
    #[cfg(test)]
    fn helper() {}
#[cfg(test)]
mod tests {
                   fn t() {
    }
}
fn b() {}
#[cfg(test)]
mod source_tests;
fn c() {}";
    let code = production_code(src);
    for kept in ["fn a()", "fn helper()", "fn b()", "fn c()"] {
        assert!(code.contains(kept), "{kept} was cut: {code}");
    }
    for cut in ["mod tests", "fn t()", "mod source_tests"] {
        assert!(!code.contains(cut), "{cut} was kept: {code}");
    }
}

/// Both of `mcp/ai_session.rs`'s kill sites sit after a method-level
/// `#[cfg(test)]`, and one after a test module: the pins below must see them.
#[test]
fn production_code_of_ai_session_holds_both_kill_sites() {
    let code = production_code(KILL_SITE_FILES[0].1);
    for site in [
        "pub async fn stop_ai_analysis(",
        "fn kill_orphaned_ai_processes(",
    ] {
        assert!(code.contains(site), "mcp/ai_session.rs: {site} not scanned");
    }
}

#[test]
fn kill_sites_keep_no_off_windows_no_op_arm() {
    for (name, source) in KILL_SITE_FILES {
        let production = production_code(source);
        // Comments included: these phrases only ever described the no-op arm.
        let lowered = production.to_lowercase();
        for phrase in [
            "no process-tree kill on this platform",
            "unchanged off windows",
        ] {
            assert!(!lowered.contains(phrase), "{name} still carries `{phrase}`");
        }
        let code = without_comment_lines(&production);
        assert!(
            !code.contains("Command::new(\"kill\")"),
            "{name} still spawns `kill`"
        );
    }
}

/// Identifiers an AI pid tracker goes by in this crate.
const TRACKER_MARKERS: [&str; 3] = ["pid_tracker", "ai_pids", "tracker.lock()"];

/// How many lines above a `.push(` may name the tracker it pushes into
/// (`if let Ok(mut pids) = tracker.lock() {` then `pids.push(pid);`).
const PUSH_WINDOW: usize = 4;

/// Lines (1-based) of `source` that push into an AI pid tracker.
fn tracker_pushes(source: &str) -> Vec<usize> {
    let code = without_comment_lines(source);
    let lines: Vec<&str> = code.lines().collect();
    let mut hits = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(".push(") {
            continue;
        }
        let from = i.saturating_sub(PUSH_WINDOW);
        if lines[from..=i]
            .iter()
            .any(|l| TRACKER_MARKERS.iter().any(|m| l.contains(m)))
        {
            hits.push(i + 1);
        }
    }
    hits
}

fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn tracker_push_detector_flags_the_raw_push_and_not_the_writer_api() {
    let raw = "if let Some(ref tracker) = pid_tracker {\n    if let Ok(mut pids) = \
               tracker.lock() {\n        pids.push(child_pid);\n    }\n}";
    assert_eq!(tracker_pushes(raw), vec![3]);
    let api = "if let Some(ref tracker) = pid_tracker {\n    \
               crate::tree_kill::track_ai_pid(tracker, child_pid);\n}";
    assert!(tracker_pushes(api).is_empty());
}

#[test]
fn only_track_ai_pid_pushes_into_an_ai_pid_tracker() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let own = src.join("tree_kill");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        files.len() > 100,
        "src walk found only {} files",
        files.len()
    );
    let mut offenders = Vec::new();
    for file in files.iter().filter(|f| !f.starts_with(&own)) {
        let source = std::fs::read_to_string(file).expect("read source file");
        for line in tracker_pushes(&source) {
            offenders.push(format!("{}:{line}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "push into an AI pid tracker outside tree_kill::track_ai_pid: {offenders:?}"
    );
}

/// Review r1, finding 8: both per-task stop endpoints go through THE helper
/// whose scoping is tested (`stop_task_run_trees`), never a drain-all.
#[test]
fn per_task_endpoints_call_the_tested_helper_only() {
    for (name, source) in [KILL_SITE_FILES[1], KILL_SITE_FILES[2]] {
        let code = without_comment_lines(&production_code(source));
        assert!(
            code.contains("tree_kill::stop_task_run_trees("),
            "{name} does not stop a task run through tree_kill::stop_task_run_trees"
        );
        for banned in [
            "kill_tracked(",
            "kill_tracked_subset(",
            "kill_tracked_with_deadline(",
        ] {
            assert!(!code.contains(banned), "{name} calls {banned}");
        }
    }
}

/// Every final-refusal phrase is carried by a message a kill path produces,
/// so a reworded message cannot silently turn a final refusal into a retry
/// or the reverse (review r1, finding 7).
#[test]
fn every_final_refusal_phrase_is_produced_somewhere() {
    let own = include_str!("mod.rs");
    let after_const = own
        .split_once("const FINAL_REFUSALS")
        .and_then(|(_, rest)| rest.split_once("];"))
        .map(|(_, rest)| rest)
        .expect("mod.rs holds FINAL_REFUSALS");
    let producers = [
        after_const,
        include_str!("unix.rs"),
        include_str!("../../../crates/pty-holder/src/tree_freeze.rs"),
    ];
    for phrase in super::FINAL_REFUSALS {
        assert!(
            producers.iter().any(|src| src.contains(phrase)),
            "no kill path produces `{phrase}`"
        );
    }
}
