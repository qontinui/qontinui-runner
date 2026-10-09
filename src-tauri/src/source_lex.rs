//! Source-lexing helpers shared by the runner's source-scanning guards.
//!
//! The guards that walk `src-tauri/src` and tell production code from test code
//! (`runner_spawn_sites`, `process_helpers`, `wedge_diagnostics`,
//! `commands::tenant`'s pin-reader scan, and the
//! `tests/coord_auth_pin.rs`, `tests/coord_schema_authorship.rs` and
//! `tests/interactive_signout_marker_guard.rs` integration gates) find test code
//! by an IN-FILE `#[cfg(test)]` span. A test module moved out of line by
//! `qontinui-claude-config/scripts/extract-rust-test-modules.py` has no such
//! span: the `#[cfg(test)]` stays on the parent's `mod tests;` line, so without
//! [`is_test_only_file`] every one of those guards would read the moved test
//! code as production. Plan `2026-10-01-oversized-source-files-owe-a-decomposition`,
//! D4 and Phase 2b.
//!
//! The predicate sees only the file itself, so it covers files that CARRY the
//! header. An out-of-line test file written by hand without it (several exist,
//! e.g. `scenarios/tests.rs`) is still scanned as production — the safe
//! direction: a spurious finding, never a hidden one. The parent's braceless
//! `#[cfg(test)] mod x;` line is the other half; the brace-tracking guards end a
//! pending `#[cfg(test)]` at such a `;` so it cannot swallow the next item.
//!
//! Compiled into the lib and bin test builds (`#[cfg(test)] mod source_lex;` in
//! both crate roots) and into the integration gates through `#[path]`, so there
//! is ONE predicate rather than a copy per guard.
//!
//! Not adopted by `ambient.rs` or `tests/workspace_assumptions_are_enumerated.rs`:
//! both already classify a file-level `#![cfg(test)]` with their own token/lexer
//! scans, which are more general (they also drop out-of-line
//! `#[cfg(test)] mod x;` targets) than this line predicate.

/// The header line the extraction codemod writes as a moved test file's first
/// line. Redundant for rustc; it is the file-local signal the guards key on.
const TEST_ONLY_FILE_HEADER: &str = "#![cfg(test)]";

/// True when `text`'s first non-blank, non-comment line is exactly
/// `#![cfg(test)]` — a file whose WHOLE content is test code.
///
/// File-local by design: a guard needs no parent lookup and no path convention.
/// Line (`//`, `///`, `//!`) and block (`/* … */`) comments and blank lines are
/// skipped; anything else decides. A leading byte-order mark is ignored.
pub(crate) fn is_test_only_file(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut in_block = false;
    for line in text.lines() {
        let mut rest = line.trim();
        loop {
            if in_block {
                match rest.split_once("*/") {
                    Some((_, after)) => {
                        in_block = false;
                        rest = after.trim_start();
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            }
            match rest.strip_prefix("/*") {
                Some(after) => {
                    in_block = true;
                    rest = after;
                }
                None => break,
            }
        }
        if rest.is_empty() || rest.starts_with("//") {
            continue;
        }
        return rest.trim_end() == TEST_ONLY_FILE_HEADER;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_test_only_file;

    #[test]
    fn the_codemod_header_marks_a_test_only_file() {
        // Exactly what `extract-rust-test-modules.py` writes.
        assert!(is_test_only_file(
            "#![cfg(test)]\n\nuse super::*;\n\n#[test]\nfn t() {}\n"
        ));
        // Comments and blank lines before it do not hide it.
        assert!(is_test_only_file(
            "\n// moved\n//! doc\n/* a\n   block */\n  #![cfg(test)]\nfn t() {}\n"
        ));
        assert!(is_test_only_file("/* one-line */ #![cfg(test)]\n"));
        assert!(is_test_only_file("\u{feff}#![cfg(test)]\n"));
    }

    #[test]
    fn anything_else_first_is_production() {
        assert!(!is_test_only_file(""));
        assert!(!is_test_only_file("// only a comment\n"));
        // An in-file span is the guards' OTHER rule, not this one.
        assert!(!is_test_only_file("#[cfg(test)]\nmod tests {}\n"));
        assert!(!is_test_only_file("#![cfg(not(test))]\nfn f() {}\n"));
        assert!(!is_test_only_file("#![cfg(any(test, debug_assertions))]\n"));
        // Only the FIRST code line counts.
        assert!(!is_test_only_file("use x;\n#![cfg(test)]\n"));
        assert!(!is_test_only_file("#![allow(dead_code)]\n#![cfg(test)]\n"));
        // Inside a comment it is prose.
        assert!(!is_test_only_file("// #![cfg(test)]\nfn f() {}\n"));
        assert!(!is_test_only_file("/*\n#![cfg(test)]\n*/\nfn f() {}\n"));
    }
}
