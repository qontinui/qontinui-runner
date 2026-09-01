//! Playwright output parsing utilities
//!
//! Contains JSON parsing and file collection utilities.
//!
//! ## ANSI stripping is not implemented here
//!
//! A local `strip_ansi_codes` used to live in this file: a per-call
//! `Regex::new(r"\x1b\[[0-9;]*m")` that matched SGR colour codes and nothing
//! else. It was CONSOLIDATED into [`crate::terminal::strip_ansi`] (plan
//! `2026-08-28-text-framing-escapes-outside-the-pty-choke-point`, Phase 2)
//! rather than kept as a second read-path stripper. Both had the same contract
//! — take untrusted terminal output, return human-readable text — and the
//! canonical one is strictly more capable (every CSI, not just `…m`; OSC, DCS,
//! SOS, PM and APC; control-byte filtering; no unterminated-sequence data
//! loss) and does not recompile a regex on every error message it touches.
//! A Playwright reporter's `error.message` is raw child-process output —
//! whatever the failing test, the browser, or a nested tool wrote — so SGR is
//! the most COMMON thing in it, not the only thing it can carry. Anything else
//! the old regex passed straight through into the UI.

use super::results::TestSpec;
use std::fs;
use std::path::PathBuf;

/// Parse Playwright JSON reporter output
///
/// Returns (tests_passed, tests_failed, tests_skipped, specs, error_message)
pub fn parse_playwright_json(output: &str) -> (u32, u32, u32, Vec<TestSpec>, Option<String>) {
    // Try to find JSON in the output (it might have other text before/after)
    let json_start = output.find('{');
    let json_end = output.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        let json_str = &output[start..=end];

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            let mut passed = 0u32;
            let mut failed = 0u32;
            let mut skipped = 0u32;
            let mut specs = Vec::new();
            let mut error_msg = None;

            // Parse suites/specs from Playwright JSON format
            if let Some(suites) = json.get("suites").and_then(|s| s.as_array()) {
                for suite in suites {
                    if let Some(suite_specs) = suite.get("specs").and_then(|s| s.as_array()) {
                        for spec in suite_specs {
                            let title = spec
                                .get("title")
                                .and_then(|t| t.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let file = spec
                                .get("file")
                                .and_then(|f| f.as_str())
                                .unwrap_or("")
                                .to_string();

                            // Get test results
                            if let Some(tests) = spec.get("tests").and_then(|t| t.as_array()) {
                                for test in tests {
                                    let status = test
                                        .get("status")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();

                                    let duration = test
                                        .get("results")
                                        .and_then(|r| r.as_array())
                                        .and_then(|r| r.first())
                                        .and_then(|r| r.get("duration"))
                                        .and_then(|d| d.as_u64())
                                        .unwrap_or(0);

                                    let test_error = test
                                        .get("results")
                                        .and_then(|r| r.as_array())
                                        .and_then(|r| r.first())
                                        .and_then(|r| r.get("error"))
                                        .and_then(|e| e.get("message"))
                                        .and_then(|m| m.as_str())
                                        .map(crate::terminal::strip_ansi);

                                    match status.as_str() {
                                        "passed" | "expected" => passed += 1,
                                        "failed" | "unexpected" => {
                                            failed += 1;
                                            if error_msg.is_none() {
                                                error_msg = test_error.clone();
                                            }
                                        }
                                        "skipped" => skipped += 1,
                                        _ => {}
                                    }

                                    specs.push(TestSpec {
                                        title: title.clone(),
                                        file: file.clone(),
                                        status,
                                        duration_ms: duration,
                                        error: test_error,
                                        retry: 0,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            return (passed, failed, skipped, specs, error_msg);
        }
    }

    // If we couldn't parse JSON, check exit status
    if output.contains("passed") && !output.contains("failed") {
        (1, 0, 0, Vec::new(), None)
    } else if output.contains("failed") {
        (
            0,
            1,
            0,
            Vec::new(),
            Some("Test execution failed - see console output".to_string()),
        )
    } else {
        (
            0,
            0,
            0,
            Vec::new(),
            Some("Could not parse test results".to_string()),
        )
    }
}

/// Collect screenshot paths from output directory (recursive)
pub fn collect_screenshots(output_dir: &PathBuf) -> Vec<String> {
    let mut screenshots = Vec::new();

    fn collect_in_dir(dir: &PathBuf, screenshots: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    collect_in_dir(&path, screenshots);
                } else if let Some(ext) = path.extension() {
                    if ext == "png" || ext == "jpg" || ext == "jpeg" || ext == "webp" {
                        if let Some(path_str) = path.to_str() {
                            screenshots.push(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    collect_in_dir(output_dir, &mut screenshots);
    screenshots
}

/// Collect error context (page snapshot) from error-context.md files
pub fn collect_error_context(output_dir: &PathBuf) -> Option<String> {
    fn find_in_dir(dir: &PathBuf) -> Option<String> {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = find_in_dir(&path) {
                        return Some(found);
                    }
                } else if path.file_name().is_some_and(|n| n == "error-context.md") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        return Some(content);
                    }
                }
            }
        }
        None
    }

    find_in_dir(output_dir)
}

/// Find trace file in output directory (recursively checks subdirs)
pub fn find_trace_file(output_dir: &PathBuf) -> Option<String> {
    fn find_in_dir(dir: &PathBuf) -> Option<String> {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    if let Some(found) = find_in_dir(&path) {
                        return Some(found);
                    }
                } else if let Some(ext) = path.extension() {
                    if ext == "zip" && path.to_str().is_some_and(|s| s.contains("trace")) {
                        return path.to_str().map(String::from);
                    }
                }
            }
        }
        None
    }

    find_in_dir(output_dir)
}

/// Find video files in output directory (recursively checks subdirs)
pub fn find_video_files(output_dir: &PathBuf) -> Vec<String> {
    let mut videos = Vec::new();

    fn find_in_dir(dir: &PathBuf, videos: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    find_in_dir(&path, videos);
                } else if let Some(ext) = path.extension() {
                    // Playwright records videos as .webm files
                    if ext == "webm" || ext == "mp4" {
                        if let Some(path_str) = path.to_str() {
                            videos.push(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    find_in_dir(output_dir, &mut videos);
    videos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stripper itself is tested on the canonical function
    /// (`crate::terminal`); what this module owes is that a reporter error
    /// message actually goes THROUGH it. The payload carries an OSC title and
    /// a cursor-motion CSI as well as SGR colour — the old local regex matched
    /// only the last of the three and leaked the other two into the UI.
    #[test]
    fn test_parse_playwright_json_strips_ansi_from_error_message() {
        let output = r#"{"suites":[{"specs":[{"title":"t","file":"a.spec.ts","tests":[{"status":"failed","results":[{"duration":7,"error":{"message":"\u001b]0;title\u0007\u001b[2K\u001b[31mError\u001b[0m: boom"}}]}]}]}]}"#;
        let (_passed, failed, _skipped, specs, error) = parse_playwright_json(output);
        assert_eq!(failed, 1);
        assert_eq!(error.as_deref(), Some("Error: boom"));
        assert_eq!(specs[0].error.as_deref(), Some("Error: boom"));
    }

    #[test]
    fn test_parse_playwright_json_passed() {
        let output = r#"{ "suites": [] }"#;
        let (passed, failed, _skipped, _specs, error) = parse_playwright_json(output);
        // No specs in the JSON, so no tests counted
        assert_eq!(passed, 0);
        assert_eq!(failed, 0);
        assert!(error.is_none());
    }

    #[test]
    fn test_parse_playwright_json_fallback_passed() {
        let output = "1 passed";
        let (passed, failed, _skipped, _specs, error) = parse_playwright_json(output);
        assert_eq!(passed, 1);
        assert_eq!(failed, 0);
        assert!(error.is_none());
    }

    #[test]
    fn test_parse_playwright_json_fallback_failed() {
        let output = "1 failed";
        let (passed, failed, _skipped, _specs, error) = parse_playwright_json(output);
        assert_eq!(passed, 0);
        assert_eq!(failed, 1);
        assert!(error.is_some());
    }
}
