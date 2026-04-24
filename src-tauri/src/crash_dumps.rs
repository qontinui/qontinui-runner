//! Startup crash-dump scanner — surfaces the most recent Rust panic the
//! runner aborted on so a restarted runner can flag itself as
//! *recently-errored* before any React code even mounts.
//!
//! # Why this exists
//!
//! Phase 3J.1/3J.2 surfaces frontend errors via the React ErrorBoundary,
//! but non-unwinding Rust panics (e.g. `Handle::current()` panics bubbling
//! across the WebView2 FFI boundary) abort the process before the React
//! layer can report anything. Tauri restarts the binary clean, the new
//! instance has no memory of the crash, `/health.derived_status` returns
//! `healthy`, and supervisors/fleet views mark the runner as recovered —
//! even though the crash may recur on the next tab click.
//!
//! This module bridges that gap by reading `crash_*.txt` files from the
//! crash-dump directory at startup. If the most recent dump is within a
//! configurable freshness window (default 10 minutes), its timestamp,
//! panic location, and panic message are stored in [`CrashDumpState`] and
//! surfaced on `/health` alongside `ui_error`. Supervisors see a runner
//! that literally just crashed, even though the current process is fine.
//!
//! # Design choices
//!
//! - Scan once at startup, not continuously. A runner that crashes at
//!   runtime is either gone (next process will re-scan) or its panic
//!   bubbled through the boundary (Phase 3J reports it). Polling disk
//!   for fresh dumps adds no new signal.
//! - Keep parsing cheap — the crash dump format is line-oriented (see
//!   `logging::write_crash_dump`); we grep for a handful of section
//!   headers and take the next non-empty line. No full parser, no
//!   backtrace extraction (backtraces in minified Windows WebView2 dumps
//!   are mostly noise anyway).
//! - Freshness window defaults to 10 minutes, overridable via env var so
//!   CI and manual-test scripts can tighten it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default freshness window — dumps older than this are ignored. Matches
/// the typical "just restarted" window; older dumps are historical.
const DEFAULT_FRESHNESS_SECS: u64 = 600;
const FRESHNESS_ENV: &str = "QONTINUI_CRASH_DUMP_FRESHNESS_SECS";

/// Maximum number of files to stat when looking for the most recent dump.
/// Bounded to avoid pathological performance if `.dev-logs/` has thousands
/// of historical dumps (which it does on long-lived dev machines — see
/// the April-17 dumps in the user's tree).
const SCAN_LIMIT: usize = 200;

/// A recent crash summary — surfaced on `/health` and usable for the
/// supervisor's fleet status. Fields mirror the Phase 3J `UiError`
/// convention (camelCase via explicit renames rather than a blanket
/// `rename_all`, to stay in sync with that module's pattern).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentCrash {
    /// Absolute path to the `crash_*.txt` file the summary was read from.
    /// Useful for ops to pull the full backtrace without round-tripping
    /// through another endpoint.
    pub file_path: String,
    /// Filesystem mtime of the dump (ISO-8601, UTC). `chrono` parses both
    /// naive + TZ-aware timestamps so supervisors can sort without
    /// parsing.
    pub reported_at: DateTime<Utc>,
    /// First non-empty line under `=== PANIC LOCATION ===` in the dump,
    /// e.g. `src-tauri\src\meta_optimizer\snapshots.rs:427:9`. `None` when
    /// the file exists but the section is missing (truncated dump).
    pub panic_location: Option<String>,
    /// First non-empty line under `=== PANIC MESSAGE ===`, e.g.
    /// `there is no reactor running, must be called from the context of a
    /// Tokio 1.x runtime`.
    pub panic_message: Option<String>,
    /// First non-empty line under `=== THREAD INFO ===` (strips the
    /// leading `Thread: ` prefix). Useful for distinguishing main-thread
    /// panics (fatal — cross WebView2 FFI) from tokio-rt-worker panics
    /// (usually recoverable but still crash-dumped).
    pub thread: Option<String>,
}

/// Shared in-memory holder for the most recent crash summary.
///
/// Populated exactly once at startup by [`CrashDumpState::scan_on_startup`];
/// subsequently cleared by [`CrashDumpState::dismiss`] when the user acks
/// the warning, or left in place for supervisor consumption.
#[derive(Debug, Default)]
pub struct CrashDumpState {
    inner: Arc<RwLock<Option<RecentCrash>>>,
}

impl CrashDumpState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Scan the crash-dump directory for a fresh dump and, if found, stash
    /// the parsed summary.
    ///
    /// Silent on empty/absent directory — a runner that has never crashed
    /// is the common case. Logs a one-line warn! when a fresh dump is
    /// adopted so the startup log trail shows why `derived_status` is
    /// errored.
    pub async fn scan_on_startup(&self, dump_dir: &Path) {
        let freshness = freshness_window();
        match find_latest_fresh_dump(dump_dir, freshness) {
            Some(crash) => {
                warn!(
                    file = %crash.file_path,
                    reported_at = %crash.reported_at,
                    location = crash.panic_location.as_deref().unwrap_or("<none>"),
                    message = crash.panic_message.as_deref().unwrap_or("<none>"),
                    "startup crash-dump scan: found a fresh crash dump; \
                     /health.derived_status will report `errored` until dismissed"
                );
                let mut guard = self.inner.write().await;
                *guard = Some(crash);
            }
            None => {
                debug!(
                    "startup crash-dump scan: no fresh dumps in {:?} (freshness window {:?})",
                    dump_dir, freshness
                );
            }
        }
    }

    /// Read a clone of the current record, if any. Cheap — same pattern
    /// as [`crate::ui_error::UiErrorState::get`].
    pub async fn get(&self) -> Option<RecentCrash> {
        self.inner.read().await.clone()
    }

    /// Dismiss the current record. The on-disk file stays intact; only
    /// the in-memory surface clears. Called by the `dismiss_recent_crash`
    /// Tauri command after the user acks the banner.
    pub async fn dismiss(&self) {
        let mut guard = self.inner.write().await;
        if let Some(prev) = guard.take() {
            info!(
                "crash-dump dismissed by user acknowledgment: {} ({})",
                prev.file_path,
                prev.panic_message.as_deref().unwrap_or("<no message>")
            );
        }
    }

    /// Test-only: directly inject a summary without hitting disk. Kept
    /// `pub(crate)` so the health-endpoint integration test can exercise
    /// the derived_status flip without manufacturing a real dump file.
    #[cfg(test)]
    pub(crate) async fn set_for_test(&self, crash: RecentCrash) {
        let mut guard = self.inner.write().await;
        *guard = Some(crash);
    }
}

fn freshness_window() -> Duration {
    std::env::var(FRESHNESS_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_FRESHNESS_SECS))
}

fn find_latest_fresh_dump(dump_dir: &Path, freshness: Duration) -> Option<RecentCrash> {
    let entries = match std::fs::read_dir(dump_dir) {
        Ok(iter) => iter,
        Err(e) => {
            debug!(
                "crash-dump scan: read_dir({:?}) failed: {} — treating as no dumps",
                dump_dir, e
            );
            return None;
        }
    };

    let now = SystemTime::now();
    let cutoff = now.checked_sub(freshness)?;

    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten().take(SCAN_LIMIT) {
        let path = entry.path();
        if !is_crash_dump_filename(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if mtime < cutoff {
            continue;
        }
        candidates.push((mtime, path));
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (mtime, path) = candidates.into_iter().next()?;
    parse_crash_dump(&path, mtime)
}

fn is_crash_dump_filename(path: &Path) -> bool {
    // Only consider `crash_*.txt` — the fixed `latest_crash.txt` alias is
    // skipped because its mtime mirrors whichever real dump was written
    // last, and we'd double-count if the two disagree on the section
    // content (they don't today, but the alias is an implementation
    // detail of `logging::write_crash_dump`, not a stable contract).
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("crash_") && name.ends_with(".txt")
}

fn parse_crash_dump(path: &Path, mtime: SystemTime) -> Option<RecentCrash> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            warn!("crash-dump scan: failed to read {:?}: {}", path, e);
            return None;
        }
    };

    let reported_at: DateTime<Utc> = DateTime::<Utc>::from(mtime);
    let panic_location = extract_section(&content, "=== PANIC LOCATION ===");
    let panic_message = extract_section(&content, "=== PANIC MESSAGE ===");
    let thread = extract_section(&content, "=== THREAD INFO ===").map(|t| {
        // Strip `Thread: ` prefix and surrounding quotes if present so
        // consumers see a clean `main` / `tokio-rt-worker`.
        t.trim_start_matches("Thread:")
            .trim()
            .trim_matches('"')
            .to_string()
    });

    Some(RecentCrash {
        file_path: path.to_string_lossy().into_owned(),
        reported_at,
        panic_location,
        panic_message,
        thread,
    })
}

/// Return the first non-empty trimmed line following a section header.
/// Returns `None` if the header is absent or no non-empty content follows.
fn extract_section(content: &str, header: &str) -> Option<String> {
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        if line.trim() == header {
            for candidate in lines.by_ref() {
                let trimmed = candidate.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tauri command — dismiss
// ---------------------------------------------------------------------------

/// Acknowledge and clear the runtime's `recent_crash` record.
///
/// The on-disk `crash_*.txt` file is left alone for forensics; only the
/// in-memory surface is cleared. Safe to call when no crash is tracked
/// (no-op).
#[tauri::command]
pub async fn dismiss_recent_crash(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<(), String> {
    app_state.crash_dumps.dismiss().await;
    Ok(())
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_crash_dumps")
        .invoke_handler(tauri::generate_handler![dismiss_recent_crash])
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_dump(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).expect("write test dump");
        path
    }

    const SAMPLE_DUMP: &str = "=== QONTINUI RUNNER CRASH DUMP ===\n\
        Timestamp: 2026-04-21T20:04:37.145070100+02:00\n\
        Version: 1.0.0\n\
        \n\
        === PANIC LOCATION ===\n\
        src-tauri\\src\\meta_optimizer\\snapshots.rs:427:9\n\
        \n\
        === PANIC MESSAGE ===\n\
        there is no reactor running, must be called from the context of a Tokio 1.x runtime\n\
        \n\
        === ENVIRONMENT ===\n\
        OS: windows\n\
        \n\
        === THREAD INFO ===\n\
        Thread: \"main\"\n\
        \n\
        === BACKTRACE ===\n\
        Backtrace [...]\n\
        \n\
        === END CRASH DUMP ===\n";

    #[test]
    fn parse_extracts_location_message_and_thread() {
        let tmp = TempDir::new().unwrap();
        let path = write_dump(tmp.path(), "crash_20260421_200437.txt", SAMPLE_DUMP);
        let crash = parse_crash_dump(&path, SystemTime::now()).expect("parse");
        assert_eq!(
            crash.panic_location.as_deref(),
            Some(r"src-tauri\src\meta_optimizer\snapshots.rs:427:9")
        );
        assert_eq!(
            crash.panic_message.as_deref(),
            Some(
                "there is no reactor running, must be called from the context of a Tokio 1.x runtime"
            )
        );
        assert_eq!(crash.thread.as_deref(), Some("main"));
    }

    #[test]
    fn parse_handles_missing_sections() {
        let tmp = TempDir::new().unwrap();
        let truncated = "=== QONTINUI RUNNER CRASH DUMP ===\nTimestamp: 2026-04-21T20:04:37Z\n";
        let path = write_dump(tmp.path(), "crash_partial.txt", truncated);
        let crash = parse_crash_dump(&path, SystemTime::now()).expect("parse");
        assert_eq!(crash.panic_location, None);
        assert_eq!(crash.panic_message, None);
        assert_eq!(crash.thread, None);
    }

    #[test]
    fn is_crash_dump_filename_accepts_only_crash_prefix() {
        assert!(is_crash_dump_filename(Path::new("/tmp/crash_20260421.txt")));
        assert!(!is_crash_dump_filename(Path::new("/tmp/latest_crash.txt")));
        assert!(!is_crash_dump_filename(Path::new("/tmp/other.txt")));
        assert!(!is_crash_dump_filename(Path::new(
            "/tmp/crash_20260421.log"
        )));
    }

    #[test]
    fn find_latest_fresh_dump_ignores_stale_files() {
        let tmp = TempDir::new().unwrap();
        let _stale = write_dump(tmp.path(), "crash_stale.txt", SAMPLE_DUMP);
        // A zero-second freshness window forces every dump — no matter how
        // recent — to be considered stale. Cheaper than mtime-munging to
        // prove the cutoff comparison short-circuits.
        let found = find_latest_fresh_dump(tmp.path(), Duration::from_secs(0));
        assert!(
            found.is_none(),
            "zero-second window should exclude every dump as stale"
        );
    }

    #[test]
    fn find_latest_fresh_dump_picks_newest_when_multiple_fresh() {
        let tmp = TempDir::new().unwrap();
        let _older = write_dump(tmp.path(), "crash_a.txt", SAMPLE_DUMP);
        std::thread::sleep(Duration::from_millis(20));
        let newer = write_dump(tmp.path(), "crash_b.txt", SAMPLE_DUMP);

        let found = find_latest_fresh_dump(tmp.path(), Duration::from_secs(3600)).expect("found");
        assert_eq!(
            found.file_path,
            newer.to_string_lossy().into_owned(),
            "should pick the newer dump"
        );
    }

    #[tokio::test]
    async fn dismiss_clears_state() {
        let state = CrashDumpState::new();
        state
            .set_for_test(RecentCrash {
                file_path: "/tmp/fake.txt".to_string(),
                reported_at: Utc::now(),
                panic_location: None,
                panic_message: None,
                thread: None,
            })
            .await;
        assert!(state.get().await.is_some());
        state.dismiss().await;
        assert!(state.get().await.is_none());
    }

    #[tokio::test]
    async fn scan_on_startup_no_op_on_missing_dir() {
        let state = CrashDumpState::new();
        state
            .scan_on_startup(Path::new("/definitely/does/not/exist/at/all"))
            .await;
        assert!(state.get().await.is_none());
    }
}
