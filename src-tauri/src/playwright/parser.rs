//! Playwright output parsing utilities
//!
//! Contains JSON parsing, ANSI stripping, and file collection utilities.

use super::results::TestSpec;
use std::fs;
use std::path::PathBuf;

/// Strip ANSI escape codes from a string
pub fn strip_ansi_codes(s: &str) -> String {
    // ANSI escape codes follow the pattern: ESC [ ... m
    // ESC is \x1b (27) or \033
    // Safe: regex pattern is a compile-time constant
    match regex::Regex::new(r"\x1b\[[0-9;]*m") {
        Ok(re) => re.replace_all(s, "").to_string(),
        Err(_) => s.to_string(), // Fallback: return original string if regex fails
    }
}

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
                                        .map(strip_ansi_codes);

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

    #[test]
    fn test_strip_ansi_codes() {
        let input = "\x1b[31mError\x1b[0m: Something failed";
        let output = strip_ansi_codes(input);
        assert_eq!(output, "Error: Something failed");
    }

    #[test]
    fn test_strip_ansi_codes_no_codes() {
        let input = "Plain text without ANSI codes";
        let output = strip_ansi_codes(input);
        assert_eq!(output, input);
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
