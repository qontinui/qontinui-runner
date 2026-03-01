//! File watcher trigger: monitors directories for file changes.
//!
//! Uses the `notify` crate (already a dependency) with built-in debouncing.
//! Applies glob pattern filters to include/exclude files.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::super::types::TriggerEvent;

/// Directories to always ignore when watching for file changes.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".cache",
    ".turbo",
    ".nuxt",
    ".svelte-kit",
    "coverage",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];

/// Start a file watcher for a trigger.
///
/// Returns the `notify::RecommendedWatcher` which must be kept alive.
pub fn start_file_watcher(
    trigger_id: String,
    paths: Vec<String>,
    patterns: Vec<String>,
    ignore_patterns: Vec<String>,
    recursive: bool,
    tx: mpsc::Sender<TriggerEvent>,
    stop_signal: Arc<AtomicBool>,
) -> Result<RecommendedWatcher, String> {
    let trigger_id_for_handler = trigger_id.clone();
    let patterns_for_handler = patterns.clone();
    let ignore_for_handler = ignore_patterns.clone();

    let mut watcher = notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
        if stop_signal.load(Ordering::SeqCst) {
            return;
        }

        match result {
            Ok(event) => {
                // Only react to modifications and creations
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
                    _ => return,
                }

                // Filter paths
                let matched_paths: Vec<String> = event
                    .paths
                    .iter()
                    .filter(|p| {
                        // Check ignored directories
                        if is_in_ignored_dir(p) {
                            return false;
                        }

                        let path_str = p.to_string_lossy().to_string();

                        // Check include patterns (if any specified, at least one must match)
                        if !patterns_for_handler.is_empty() {
                            let matches_include = patterns_for_handler.iter().any(|pattern| {
                                glob_match::glob_match(pattern, &path_str)
                                    || p.file_name()
                                        .map(|n| {
                                            glob_match::glob_match(pattern, &n.to_string_lossy())
                                        })
                                        .unwrap_or(false)
                            });
                            if !matches_include {
                                return false;
                            }
                        }

                        // Check exclude patterns
                        if ignore_for_handler.iter().any(|pattern| {
                            glob_match::glob_match(pattern, &path_str)
                                || p.file_name()
                                    .map(|n| glob_match::glob_match(pattern, &n.to_string_lossy()))
                                    .unwrap_or(false)
                        }) {
                            return false;
                        }

                        true
                    })
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();

                if matched_paths.is_empty() {
                    return;
                }

                debug!(
                    "File watcher trigger '{}': {} files changed",
                    trigger_id_for_handler,
                    matched_paths.len()
                );

                let mut variables = HashMap::new();
                variables.insert("changed_files".to_string(), matched_paths.join(", "));
                variables.insert("changed_count".to_string(), matched_paths.len().to_string());

                let event = TriggerEvent {
                    trigger_id: trigger_id_for_handler.clone(),
                    event_type: "file_changed".to_string(),
                    event_data: serde_json::json!({
                        "files": matched_paths,
                        "event_kind": format!("{:?}", event.kind),
                    }),
                    variables,
                    chain_depth: 0,
                };

                // Use try_send since we're in a sync callback
                if let Err(e) = tx.try_send(event) {
                    warn!("Failed to send file watch event: {}", e);
                }
            }
            Err(e) => {
                warn!("File watcher error: {}", e);
            }
        }
    })
    .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    // Watch each configured path
    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };

    for path in &paths {
        let watch_path = PathBuf::from(path);
        if watch_path.exists() {
            watcher
                .watch(&watch_path, mode)
                .map_err(|e| format!("Failed to watch path '{}': {}", path, e))?;
            info!("File watcher: watching '{}'", path);
        } else {
            warn!("File watcher: path '{}' does not exist, skipping", path);
        }
    }

    Ok(watcher)
}

/// Check if a path is inside an ignored directory.
fn is_in_ignored_dir(path: &std::path::Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(name_str) = name.to_str() {
                if IGNORED_DIRS.contains(&name_str) {
                    return true;
                }
            }
        }
    }
    false
}
