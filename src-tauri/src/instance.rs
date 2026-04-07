//! Runner instance identity helpers.
//!
//! The supervisor sets `QONTINUI_INSTANCE_NAME` when spawning non-primary
//! runners (test runners, themed runners, etc.). This module centralizes the
//! detection and provides a path-segment helper so per-runner on-disk state
//! can be isolated without touching shared state (settings.json,
//! auth_tokens.enc, PostgreSQL).
//!
//! Primary runner: `data_subdir()` returns `None` — existing paths unchanged.
//! Secondary:      `data_subdir()` returns `Some("instance-<sanitized>")`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The raw instance name from the env, if set and non-empty.
pub fn instance_name() -> Option<String> {
    std::env::var("QONTINUI_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
}

/// True when this runner was launched as a non-primary instance.
///
/// Note: this is a weaker check than `process_capture::primary_proxy::is_secondary`
/// — it only requires the instance name, not a primary port — because path
/// isolation should kick in even when the secondary has no primary to proxy to.
pub fn is_secondary() -> bool {
    instance_name().is_some()
}

/// Returns the per-instance path segment, or `None` for the primary runner.
///
/// Primary:   `None`                            → callers leave paths alone
/// Secondary: `Some("instance-<sanitized>")`    → callers append to per-runner dirs
pub fn data_subdir() -> Option<String> {
    instance_name().map(|n| format!("instance-{}", sanitize(&n)))
}

/// Append the instance subdir to `base` when this runner is a secondary.
/// Returns `base` unchanged for the primary runner.
pub fn scope_path(base: &Path) -> PathBuf {
    match data_subdir() {
        Some(sub) => base.join(sub),
        None => base.to_path_buf(),
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(sanitize("test-runner_1"), "test-runner_1");
        assert_eq!(sanitize("abc/def"), "abc_def");
        assert_eq!(sanitize("weird name!"), "weird_name_");
    }
}
