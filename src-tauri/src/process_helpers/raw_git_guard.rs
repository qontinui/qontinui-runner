//! Every production git spawn goes through [`super::no_window`] /
//! [`super::tokio_no_window`],
//! because that is where BOTH git postures are applied: the prompt-closing env
//! and the repository-local scrub. A raw `Command::new("git")` skips both — it
//! can hang on a credential prompt, and under a `GIT_DIR` inherited from the
//! runner's launcher it reads and WRITES the launcher's repo instead of the one
//! `-C` names (`git reset --hard`, `worktree remove`, `git config --local`).
//!
//! A deliberate exception carries a `git-env-ok: <reason>` marker on the line
//! or just above it. Today there is exactly one: `bin/qontinui_cli.rs`, a
//! console tool that acts on the user's OWN shell repo, where the inherited
//! environment is what the user means.
//!
//! Test code follows the same rule. A fixture's `git -C <tempdir> init/add/
//! commit` run from inside a git hook inherits the hook's `GIT_DIR` /
//! `GIT_INDEX_FILE` and acts on the hook's repository instead of the tempdir.
//! So a second arm, [`no_git_spawn_anywhere_bypasses_the_chokepoint`], reads
//! EVERY line of `src/` and `tests/` (no test-region skipping) and pins the
//! total of marked exceptions, test sites included, at
//! [`EXPECTED_MARKED_ANYWHERE`].

use super::source_scan::{production_lines, rs_files};
use std::path::PathBuf;

/// How far above a raw spawn a `git-env-ok:` marker still covers it — a
/// marker comment may run to several lines.
const MARKER_WINDOW: usize = 6;

/// Is this line a raw git spawn? Whitespace is removed first, so
/// `Command::new( "git" )`, `Command::new(r"git")`, `"git.exe"` and
/// `"git".to_string()` all count.
fn is_raw_git_spawn(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    [
        "Command::new(\"git",
        "Command::new(r\"git",
        "Command::new(r#\"git",
    ]
    .iter()
    .any(|needle| {
        compact.match_indices(needle).any(|(i, _)| {
            let rest = compact.get(i + needle.len()..).unwrap_or("");
            rest.starts_with('"')
                || rest.starts_with(".exe\"")
                || rest.starts_with("\"#")
                || rest.starts_with(".exe\"#")
        })
    })
}

/// Returns `(unmarked, marked)`: the 1-based lines of raw git spawns in
/// `src`'s production text without a `git-env-ok:` marker, and the count
/// of those that carry one.
pub(super) fn raw_git_spawns(src: &str) -> (Vec<usize>, usize) {
    let lines = production_lines(src);
    let mut unmarked = Vec::new();
    let mut marked = 0;
    for (idx, (n, line)) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") || !is_raw_git_spawn(line) {
            continue;
        }
        let start = idx.saturating_sub(MARKER_WINDOW);
        if lines[start..=idx]
            .iter()
            .any(|(_, l)| l.contains("git-env-ok:"))
        {
            marked += 1;
        } else {
            unmarked.push(*n);
        }
    }
    (unmarked, marked)
}

/// The marked raw git spawns allowed ANYWHERE under `src-tauri/src` and
/// `src-tauri/tests`, test code included. Each is a reviewed exception; a new
/// one is an edit to this number, not a free comment. The 8 sites:
///
/// 1. `src/bin/qontinui_cli.rs` `git_stdout` — the user's own shell repo.
/// 2. `src/git_trunk.rs` — unscrubbed control in the re-exec child.
/// 3. `src/agent_worktree/census.rs` — unscrubbed control in the re-exec child.
/// 4. `src/build_drift.rs` — unscrubbed control in the re-exec child.
/// 5. `src/mcp/probe_executor.rs` — unscrubbed control in the re-exec child.
/// 6. `src/build_drift.rs` — sets `GIT_DIR` itself to toggle the scrub.
/// 7. `src/git_posture.rs` — never spawned; unit-tests
///    `scrub_repo_local_git_env` on a raw builder.
/// 8. `src/ci_node/sibling_git_env.rs` — never spawned; unit-tests
///    `isolate_std_git_command` on a raw builder.
const EXPECTED_MARKED_ANYWHERE: usize = 8;

/// Like [`raw_git_spawns`], but over EVERY line of `src` — no
/// `production_lines`, no test-region parsing. Lines whose trimmed start is
/// `//` are skipped. Returns `(unmarked, marked)` as 1-based line numbers.
fn raw_git_spawns_all_lines(src: &str) -> (Vec<usize>, Vec<usize>) {
    let lines: Vec<&str> = src.lines().collect();
    let mut unmarked = Vec::new();
    let mut marked = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") || !is_raw_git_spawn(line) {
            continue;
        }
        let start = idx.saturating_sub(MARKER_WINDOW);
        if lines[start..=idx].iter().any(|l| l.contains("git-env-ok:")) {
            marked.push(idx + 1);
        } else {
            unmarked.push(idx + 1);
        }
    }
    (unmarked, marked)
}

#[test]
fn no_production_git_spawn_bypasses_the_chokepoint() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(
        files.len() > 100,
        "walked only {} files under {} — the guard scanned nothing",
        files.len(),
        root.display()
    );
    let mut violations = Vec::new();
    let mut marked_cli = 0;
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        let (unmarked, marked) = raw_git_spawns(&src);
        if rel == "bin/qontinui_cli.rs" {
            marked_cli = marked;
        } else if marked > 0 {
            violations.push(format!(
                "{rel}: {marked} git-env-ok exception(s) outside the one allowlisted file"
            ));
        }
        for n in unmarked {
            violations.push(format!("{rel}:{n}"));
        }
    }
    assert_eq!(
        marked_cli, 1,
        "bin/qontinui_cli.rs must carry exactly one marked raw git spawn (git_stdout) — \
         if it moved, went away or gained a sibling, update this guard's allowlist note"
    );
    assert!(
        violations.is_empty(),
        "raw git spawn(s) that bypass process_helpers::no_window / tokio_no_window \
         (the prompt posture AND the repository-local scrub):\n{}\n\nBuild them with \
         `crate::process_helpers::no_window(\"git\")` instead. A deliberate exception \
         carries a `// git-env-ok: <reason>` comment on the line or just above it.",
        violations.join("\n")
    );
}

/// The detector itself: a raw spawn is caught, a marked one and a test-only
/// one are not. Without this the guard could pass by matching nothing.
#[test]
fn the_detector_catches_a_raw_spawn_and_honours_the_marker() {
    let raw = "fn f() {\n    let c = std::process::Command::new(\"git\");\n}\n";
    assert_eq!(raw_git_spawns(raw), (vec![2], 0));
    let marked = "fn f() {\n    // git-env-ok: user's own repo,\n    // a second line,\n    \
                  // and a third.\n    let c = Command::new(\"git\");\n}\n";
    assert_eq!(raw_git_spawns(marked), (vec![], 1));
    let test_mod = "fn f() {}\n#[cfg(test)]\nmod t { fn g() { Command::new(\"git\"); } }\n";
    assert_eq!(raw_git_spawns(test_mod), (vec![], 0));
    // A test-only helper ITEM does not hide the production code after it.
    let after_helper = "#[cfg(test)]\nfn helper() {}\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(after_helper), (vec![3], 0));
    // Neither does a test module in the middle of a file, of any
    // visibility — while its own raw spawns stay out of the scan.
    let mid_file = "#[cfg(test)]\npub(crate) mod support {\n    fn g() {\n        \
                    Command::new(\"git\");\n    }\n}\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(mid_file), (vec![7], 0));
    // A test-only `use` ends at its `;`, and an attribute between the
    // cfg and the item does not end the skip.
    let use_then_prod =
        "#[cfg(test)]\n#[allow(unused)]\nuse std::fmt;\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(use_then_prod), (vec![4], 0));
    // A multi-line test-only `static` ends at its `;` — the production
    // code after it is scanned.
    let static_then_prod = "#[cfg(test)]\nstatic S: Vec<u8> =\n    Vec::new();\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(static_then_prod), (vec![4], 0));
    // A multi-line attribute is not mistaken for the item's head.
    let multi_attr = "#[cfg(test)]\n#[expect(\n    clippy::x,\n    reason = \"r\"\n)]\nstatic S: u8 =\n    0;\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(multi_attr), (vec![8], 0));
    // A test-only fn whose signature spans lines ends at its closing
    // `}`, not at the first `;` in its body.
    let multi_sig = "#[cfg(test)]\nfn f(\n    a: u8,\n) {\n    let x = 1;\n    Command::new(\"git\");\n}\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(multi_sig), (vec![8], 0));
    // A braceless head whose initializer line carries balanced braces
    // and ends in `;` ends there.
    let struct_init = "#[cfg(test)]\nstatic S: Foo =\n    Foo { a: 1 };\nfn prod() { Command::new(\"git\"); }\n";
    assert_eq!(raw_git_spawns(struct_init), (vec![4], 0));
    for spelling in [
        "tokio::process::Command::new(\"git\")",
        "Command::new( \"git\" )",
        "Command::new(\"git.exe\")",
        "Command::new(r\"git\")",
        "Command::new(\"git\".to_string())",
    ] {
        assert_eq!(
            raw_git_spawns(&format!("fn f() {{ {spelling}; }}\n")),
            (vec![1], 0),
            "{spelling} must be caught"
        );
    }
    for not_git in ["Command::new(\"gitk\")", "Command::new(\"git-lfs\")"] {
        assert_eq!(
            raw_git_spawns(&format!("fn f() {{ {not_git}; }}\n")),
            (vec![], 0),
            "{not_git} is not git"
        );
    }
}

/// Every git spawn under `src-tauri/src` AND `src-tauri/tests`, test code
/// included, goes through the chokepoint: a test fixture run from inside a git
/// hook otherwise inherits the hook's `GIT_DIR` / `GIT_INDEX_FILE` and writes
/// the hook's repository. The marked exceptions are pinned at
/// [`EXPECTED_MARKED_ANYWHERE`].
#[test]
fn no_git_spawn_anywhere_bypasses_the_chokepoint() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    let mut marked_sites = Vec::new();
    for (sub, min_files) in [("src", 100), ("tests", 10)] {
        let root = manifest.join(sub);
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        assert!(
            files.len() > min_files,
            "walked only {} files under {} — the guard scanned nothing",
            files.len(),
            root.display()
        );
        files.sort();
        for file in files {
            let rel = file
                .strip_prefix(&manifest)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            let Ok(src) = std::fs::read_to_string(&file) else {
                continue;
            };
            let (unmarked, marked) = raw_git_spawns_all_lines(&src);
            violations.extend(unmarked.into_iter().map(|n| format!("{rel}:{n}")));
            marked_sites.extend(marked.into_iter().map(|n| format!("{rel}:{n}")));
        }
    }
    assert!(
        violations.is_empty(),
        "{} raw git spawn(s) under src-tauri/src or src-tauri/tests (test code \
         included) bypass process_helpers::no_window (the prompt posture AND the \
         repository-local scrub):\n{}\n\nBuild them with \
         `crate::process_helpers::no_window(\"git\")` under src/, or \
         `qontinui_runner_lib::process_helpers::no_window(\"git\")` under tests/. A \
         deliberate exception carries a `// git-env-ok: <reason>` comment within \
         {MARKER_WINDOW} lines above it AND an update to EXPECTED_MARKED_ANYWHERE.",
        violations.len(),
        violations.join("\n")
    );
    assert_eq!(
        marked_sites.len(),
        EXPECTED_MARKED_ANYWHERE,
        "expected exactly {EXPECTED_MARKED_ANYWHERE} `git-env-ok:` marked raw git \
         spawns across src-tauri/src and src-tauri/tests, found {}:\n{}\n\nA new \
         deliberate spawn is a reviewed edit to EXPECTED_MARKED_ANYWHERE and its site \
         list; a removed one lowers it.",
        marked_sites.len(),
        marked_sites.join("\n")
    );
}

/// The all-lines detector: a spawn inside a `#[cfg(test)] mod` IS reported
/// (unlike the production arm), a marked one counts as marked, and a `//`
/// comment line is skipped.
#[test]
fn the_all_lines_detector_reports_test_code_and_honours_the_marker() {
    let test_mod = "fn f() {}\n#[cfg(test)]\nmod t {\n    fn g() { Command::new(\"git\"); }\n}\n";
    assert_eq!(raw_git_spawns_all_lines(test_mod), (vec![4], vec![]));
    // The production arm still skips the same text.
    assert_eq!(raw_git_spawns(test_mod), (vec![], 0));
    let marked = "#[cfg(test)]\nmod t {\n    fn g() {\n        // git-env-ok: re-exec control,\n        \
                  // reads the inherited decoy.\n        Command::new(\"git\");\n    }\n}\n";
    assert_eq!(raw_git_spawns_all_lines(marked), (vec![], vec![6]));
    let commented = "#[cfg(test)]\nmod t {\n    // let c = Command::new(\"git\");\n    \
                     /// Command::new(\"git\")\n}\n";
    assert_eq!(raw_git_spawns_all_lines(commented), (vec![], vec![]));
    // A marker further than MARKER_WINDOW lines above does not cover it.
    let far = format!(
        "// git-env-ok: too far.\n{}fn g() {{ Command::new(\"git\"); }}\n",
        "\n".repeat(MARKER_WINDOW)
    );
    assert_eq!(raw_git_spawns_all_lines(&far), (vec![MARKER_WINDOW + 2], vec![]));
}
