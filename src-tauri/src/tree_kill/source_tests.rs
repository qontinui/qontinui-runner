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

fn production_half(source: &str) -> &str {
    source
        .split("#[cfg(test)]")
        .next()
        .expect("a file has a production section")
}

#[test]
fn kill_sites_keep_no_off_windows_no_op_arm() {
    for (name, source) in KILL_SITE_FILES {
        let production = production_half(source);
        // Comments included: these phrases only ever described the no-op arm.
        let lowered = production.to_lowercase();
        for phrase in [
            "no process-tree kill on this platform",
            "unchanged off windows",
        ] {
            assert!(!lowered.contains(phrase), "{name} still carries `{phrase}`");
        }
        let code = without_comment_lines(production);
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
