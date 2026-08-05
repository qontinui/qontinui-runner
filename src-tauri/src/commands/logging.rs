//! Logging commands for AI output and render logging persistence
//!
//! Provides commands for persisting AI output logs to disk for use by
//! the QA feedback loop in `/analyze-automation`.
//!
//! Also provides render logging for UI component testing - components log
//! what data they render, and Python tests can read these logs to verify
//! the UI displays the correct information.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{error, info, warn};

use super::CommandResponse;

/// AI output entry structure (matches TypeScript AiOutputEntry)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AiOutputEntry {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Parent task run ID (matches task_runs.id in database)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<String>,
    /// Session ID for grouping output (may include phase suffix like "-agentic-1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Human-readable session/workflow name (the task title)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Workflow phase: setup, verification, agentic, or completion
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Iteration number within the phase (1, 2, 3...)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_iteration: Option<u32>,
    /// Path to screenshot file (relative to .dev-logs/)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// Screenshot width in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_width: Option<i32>,
    /// Screenshot height in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_height: Option<i32>,
}

/// Get the path to the AI output log file
fn get_ai_output_log_path() -> PathBuf {
    crate::paths::get_ai_output_jsonl_path()
}

/// Maximum AI output log file size before rotation (50 MB).
const AI_OUTPUT_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// The single sidecar a rotated JSONL log is moved aside to: `<name>.1`.
///
/// One generation is kept. Canonical here so the writer ([`AiOutputSink`]) and
/// every reader agree on the name.
pub fn rotation_sidecar_path(path: &std::path::Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "ai-output.jsonl".to_string());
    name.push_str(".1");
    path.with_file_name(name)
}

/// The full read set for a rotated JSONL log, OLDEST GENERATION FIRST: the
/// `.1` sidecar (when it exists) followed by the live file.
///
/// Rotation renames the live file aside instead of truncating it in place, so a
/// reader that opens only the live path sees (near-)nothing immediately after a
/// rotation even though every line still exists on disk. Concatenating the
/// sidecar ahead of the live file restores the chronological stream. Returns
/// only paths that exist, so the result may be empty.
pub fn rotated_log_read_set(path: &std::path::Path) -> Vec<PathBuf> {
    let mut set = Vec::with_capacity(2);
    let sidecar = rotation_sidecar_path(path);
    if sidecar.exists() {
        set.push(sidecar);
    }
    if path.exists() {
        set.push(path.to_path_buf());
    }
    set
}

/// Bound on the AI-output writer's queue. Deep enough to absorb a full
/// streaming burst (a busy AI session emits a few hundred lines/sec) without
/// ever blocking the emitting thread; a queue this deep only fills if the disk
/// has effectively stopped, in which case dropping is the correct failure mode
/// (mirrors `tracing_appender::non_blocking`'s lossy default, which the
/// runner's own file log already runs on).
const AI_OUTPUT_LOG_QUEUE_CAP: usize = 8192;

/// One message for the AI-output writer thread.
enum AiLogMsg {
    /// Append this pre-serialized JSONL line (no trailing newline).
    Line(String),
    /// Close + delete the file and start a fresh one on the next line.
    Clear,
    /// Barrier: every line queued before this one has been written and synced.
    /// The worker acknowledges on the enclosed channel. Used by
    /// [`flush_ai_output_log`] at process exit.
    Flush(std::sync::mpsc::SyncSender<()>),
}

/// How long process exit waits for the AI-output writer to drain.
///
/// Deliberately short: a flush that cannot complete in this window means the
/// disk is wedged, and blocking shutdown on it is worse than losing the tail.
const AI_OUTPUT_LOG_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Sender half of the AI-output writer queue. Initialized on first use.
static AI_OUTPUT_LOG_TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<AiLogMsg>> =
    std::sync::OnceLock::new();

/// Count of lines dropped because the queue was full, so the loss is visible
/// rather than silent. Logged (rate-limited) by the next successful send.
static AI_OUTPUT_LOG_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The single writer that owns the `ai-output.jsonl` handle.
///
/// ## Why this exists (correctness, not just latency)
///
/// This file previously had NO synchronization of any kind: every caller —
/// `emit_ai_output` on the AI hot path, three MCP/workflow call sites, and the
/// Tauri command invoked from the frontend — independently opened the shared
/// file, and any of them could trip the 50 MB threshold and start a full
/// `read_to_string` → rebuild → `fs::write` rewrite while others were still
/// appending. Appends interleaved with that rewrite were lost outright.
///
/// Now exactly one thread ever touches the file, it holds the handle open
/// (no open/close per line), and rotation is a rename to a single sidecar
/// (`ai-output.jsonl.1`) instead of an in-line 50 MB read + rewrite — so the
/// window in which a concurrent append could be lost does not exist.
struct AiOutputSink {
    path: PathBuf,
    file: Option<std::fs::File>,
    /// Bytes written to the currently-open file (its length at open time plus
    /// everything appended since), so the size check costs no `metadata` call
    /// per line.
    written: u64,
}

impl AiOutputSink {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            written: 0,
        }
    }

    /// Sidecar the live file is rotated into. Exactly one generation is kept —
    /// this replaces the old "keep the last 2000 lines" truncation, and keeps
    /// strictly MORE history than it did. Readers pick it up via
    /// [`rotated_log_read_set`].
    fn sidecar(&self) -> PathBuf {
        rotation_sidecar_path(&self.path)
    }

    fn ensure_open(&mut self) -> std::io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    /// Move the live file aside and start a fresh one. O(1) — a rename, not a
    /// read + rewrite.
    fn rotate(&mut self) {
        self.file = None; // drop the handle first (Windows rename needs it closed)
        let sidecar = self.sidecar();
        let _ = fs::remove_file(&sidecar);
        match fs::rename(&self.path, &sidecar) {
            Ok(()) => info!(
                "AI output log reached {} MB — rotated to {}",
                AI_OUTPUT_LOG_MAX_BYTES / (1024 * 1024),
                sidecar.display()
            ),
            Err(e) => {
                warn!("Failed to rotate AI output log: {} — truncating instead", e);
                let _ = fs::remove_file(&self.path);
            }
        }
        self.written = 0;
    }

    fn write_line(&mut self, line: &str) {
        if let Err(e) = self.ensure_open() {
            warn!("Failed to open AI output log file: {}", e);
            return;
        }
        let needed = line.len() as u64 + 1;
        if self.written + needed > AI_OUTPUT_LOG_MAX_BYTES {
            self.rotate();
            if let Err(e) = self.ensure_open() {
                warn!("Failed to reopen AI output log after rotation: {}", e);
                return;
            }
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if let Err(e) = writeln!(file, "{}", line) {
            warn!("Failed to write to AI output log: {}", e);
            // Force a reopen on the next line — the handle may be dead.
            self.file = None;
            return;
        }
        self.written += needed;
    }

    /// Push everything written so far all the way to disk. Called on the
    /// [`AiLogMsg::Flush`] barrier at process exit; a no-op when the file was
    /// never opened.
    fn sync(&mut self) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if let Err(e) = file.flush() {
            warn!("Failed to flush AI output log: {}", e);
        }
        if let Err(e) = file.sync_all() {
            warn!("Failed to sync AI output log to disk: {}", e);
        }
    }

    fn clear(&mut self) {
        self.file = None;
        self.written = 0;
        if self.path.exists() {
            match fs::remove_file(&self.path) {
                Ok(()) => info!("Cleared AI output log"),
                Err(e) => error!("Failed to clear AI output log: {}", e),
            }
        }
        let _ = fs::remove_file(self.sidecar());
    }
}

fn ai_output_log_worker(rx: std::sync::mpsc::Receiver<AiLogMsg>) {
    let mut sink = AiOutputSink::new(get_ai_output_log_path());
    while let Ok(msg) = rx.recv() {
        match msg {
            AiLogMsg::Line(line) => sink.write_line(&line),
            AiLogMsg::Clear => sink.clear(),
            AiLogMsg::Flush(ack) => {
                sink.sync();
                // The receiver is gone when the waiter timed out — the flush
                // still happened, nobody is listening any more.
                let _ = ack.send(());
            }
        }
    }
}

/// Drain the AI-output writer queue and wait (briefly) for it to land on disk.
///
/// Every append is queued to a background thread that owns the file handle, and
/// the static sender never drops — so nothing else would ever make the worker
/// return, and a quit or crash mid-burst silently loses the queued tail:
/// exactly the part needed to diagnose the crash. Called from
/// `RunEvent::Exit` and the force-exit watchdog. Same shape as the
/// `tracing_appender` `WorkerGuard` the runner's file log already relies on.
///
/// No-ops when the writer was never started (nothing was ever logged), and
/// never blocks longer than [`AI_OUTPUT_LOG_FLUSH_TIMEOUT`].
pub fn flush_ai_output_log() {
    // `get`, not `ai_output_log_tx()`: do not spawn a writer thread at exit
    // just to flush a queue that does not exist.
    let Some(tx) = AI_OUTPUT_LOG_TX.get() else {
        return;
    };
    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let deadline = std::time::Instant::now() + AI_OUTPUT_LOG_FLUSH_TIMEOUT;

    // A blocking `send` would be bounded by the worker draining the queue —
    // which is the very thing being waited on — so a wedged worker would hang
    // shutdown forever. Retry `try_send` inside the same deadline instead: a
    // full queue during a burst clears in milliseconds, a dead worker does not.
    let mut pending = AiLogMsg::Flush(ack_tx);
    loop {
        match tx.try_send(pending) {
            Ok(()) => break,
            Err(std::sync::mpsc::TrySendError::Full(msg))
                if std::time::Instant::now() < deadline =>
            {
                pending = msg;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                warn!(
                    "Could not queue AI output log flush ({}) — queued lines may be lost",
                    e
                );
                return;
            }
        }
    }

    // Always give the worker a real window even if queueing ate the budget.
    let remaining = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(std::time::Duration::from_millis(200));
    match ack_rx.recv_timeout(remaining) {
        Ok(()) => info!("AI output log flushed"),
        Err(e) => warn!(
            "AI output log did not flush within {:?} ({}) — queued lines may be lost",
            AI_OUTPUT_LOG_FLUSH_TIMEOUT, e
        ),
    }
}

/// The writer queue, spawning the background thread on first use.
///
/// Returns `None` if the thread could not be spawned — callers then fall back
/// to a direct synchronous append so no line is lost to a thread-spawn failure.
fn ai_output_log_tx() -> Option<&'static std::sync::mpsc::SyncSender<AiLogMsg>> {
    // `OnceLock::get_or_init` cannot fail, so a failed spawn must not populate
    // the cell — check the spawn first and only then install the sender.
    if let Some(tx) = AI_OUTPUT_LOG_TX.get() {
        return Some(tx);
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<AiLogMsg>(AI_OUTPUT_LOG_QUEUE_CAP);
    match std::thread::Builder::new()
        .name("ai-output-log".to_string())
        .spawn(move || ai_output_log_worker(rx))
    {
        Ok(_) => {
            // A racing initializer may have won; either sender is equivalent
            // (the loser's worker thread simply parks on an empty channel until
            // its sender drops).
            let _ = AI_OUTPUT_LOG_TX.set(tx);
            AI_OUTPUT_LOG_TX.get()
        }
        Err(e) => {
            error!("Failed to spawn AI output log writer thread: {}", e);
            None
        }
    }
}

/// Direct, synchronous append — the fallback used only when the writer thread
/// could not be started at all, or died.
///
/// Enforces the SAME 50 MB cap as [`AiOutputSink::write_line`]: without it a
/// runner stuck in the fallback state grows the log without bound, since the
/// writer thread that owns rotation is exactly what is missing here.
fn append_ai_output_line_direct(line: &str) -> std::io::Result<()> {
    append_line_direct_at(&get_ai_output_log_path(), line, AI_OUTPUT_LOG_MAX_BYTES)
}

/// [`append_ai_output_line_direct`] against an explicit path + cap, so the
/// rotation branch is testable without writing 50 MB.
fn append_line_direct_at(
    log_path: &std::path::Path,
    line: &str,
    max_bytes: u64,
) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let len = fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);
    if len + line.len() as u64 + 1 > max_bytes {
        let sidecar = rotation_sidecar_path(log_path);
        let _ = fs::remove_file(&sidecar);
        if let Err(e) = fs::rename(log_path, &sidecar) {
            warn!(
                "Failed to rotate AI output log (direct append path): {} — truncating instead",
                e
            );
            let _ = fs::remove_file(log_path);
        } else {
            info!(
                "AI output log reached {} bytes — rotated to {}",
                max_bytes,
                sidecar.display()
            );
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", line)
}

/// Delete BOTH generations of the AI-output log without going through the
/// writer queue — the fallback for [`clear_ai_output_log`] when the queue is
/// unavailable.
///
/// Deleting only the live file would leave up to 50 MB of the previous run in
/// the sidecar, which readers now concatenate — so the "cleared" log would
/// still serve old output. Mirrors [`AiOutputSink::clear`].
fn clear_ai_output_files_at(log_path: &std::path::Path) -> Result<(), String> {
    if log_path.exists() {
        if let Err(e) = fs::remove_file(log_path) {
            error!("Failed to clear AI output log: {}", e);
            return Err(format!("Failed to clear log: {}", e));
        }
    }
    let sidecar = rotation_sidecar_path(log_path);
    if sidecar.exists() {
        if let Err(e) = fs::remove_file(&sidecar) {
            error!("Failed to clear rotated AI output log: {}", e);
            return Err(format!("Failed to clear rotated log: {}", e));
        }
    }
    Ok(())
}

/// Append an AI output entry to the log file.
///
/// Non-blocking: the line is serialized on the caller's thread (so a
/// serialization error is still reported synchronously) and handed to the
/// single background writer that owns the file handle. The writer rotates the
/// file to a sidecar once it passes `AI_OUTPUT_LOG_MAX_BYTES`.
#[tauri::command]
pub fn append_ai_output_log(entry: AiOutputEntry) -> CommandResponse {
    // Serialize to JSON
    let json_line = match serde_json::to_string(&entry) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize AI output entry: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to serialize: {}", e)),
                data: None,
            };
        }
    };

    let Some(tx) = ai_output_log_tx() else {
        return match append_ai_output_line_direct(&json_line) {
            Ok(()) => CommandResponse {
                success: true,
                message: None,
                data: None,
            },
            Err(e) => {
                error!("Failed to write to AI output log: {}", e);
                CommandResponse {
                    success: false,
                    message: Some(format!("Failed to write: {}", e)),
                    data: None,
                }
            }
        };
    };

    use std::sync::atomic::Ordering;
    use std::sync::mpsc::TrySendError;
    match tx.try_send(AiLogMsg::Line(json_line)) {
        Ok(()) => {
            let dropped = AI_OUTPUT_LOG_DROPPED.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                warn!(
                    "AI output log writer queue was full — dropped {} line(s)",
                    dropped
                );
            }
            CommandResponse {
                success: true,
                message: None,
                data: None,
            }
        }
        Err(TrySendError::Full(_)) => {
            AI_OUTPUT_LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
            CommandResponse {
                success: false,
                message: Some("AI output log queue full — line dropped".to_string()),
                data: None,
            }
        }
        Err(TrySendError::Disconnected(msg)) => {
            // The writer thread died; fall back to a direct append so the line
            // still lands.
            let AiLogMsg::Line(line) = msg else {
                unreachable!("only Line is sent on this path")
            };
            match append_ai_output_line_direct(&line) {
                Ok(()) => CommandResponse {
                    success: true,
                    message: None,
                    data: None,
                },
                Err(e) => {
                    error!("Failed to write to AI output log: {}", e);
                    CommandResponse {
                        success: false,
                        message: Some(format!("Failed to write: {}", e)),
                        data: None,
                    }
                }
            }
        }
    }
}

/// Clear the AI output log file (called when runner starts manually).
///
/// Routed through the same writer queue as appends so it is ordered against
/// them and so the writer — the only holder of the file handle — is the one
/// that closes and deletes it.
#[tauri::command]
pub fn clear_ai_output_log() -> CommandResponse {
    let fallback_needed = match ai_output_log_tx() {
        Some(tx) => tx.send(AiLogMsg::Clear).is_err(),
        None => true,
    };
    if fallback_needed {
        if let Err(message) = clear_ai_output_files_at(&get_ai_output_log_path()) {
            return CommandResponse {
                success: false,
                message: Some(message),
                data: None,
            };
        }
    }

    CommandResponse {
        success: true,
        message: Some("AI output log cleared".to_string()),
        data: None,
    }
}

/// Get the path to the AI output log file
#[tauri::command]
pub fn get_ai_output_log_path_cmd() -> CommandResponse {
    let log_path = get_ai_output_log_path();
    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "path": log_path.to_string_lossy()
        })),
    }
}

/// Load all AI output entries from the log file
/// Used to restore conversation history on app startup
#[tauri::command]
pub fn load_ai_output_log() -> CommandResponse {
    let log_path = get_ai_output_log_path();

    // Both generations, oldest first — a rotation must not blank the restored
    // history (see `rotated_log_read_set`).
    let read_set = rotated_log_read_set(&log_path);
    if read_set.is_empty() {
        info!("No AI output log file exists, returning empty history");
        return CommandResponse {
            success: true,
            message: Some("No history file exists".to_string()),
            data: Some(serde_json::json!({
                "entries": []
            })),
        };
    }

    let mut entries: Vec<AiOutputEntry> = Vec::new();
    let mut line_number = 0;

    for part in &read_set {
        let file = match fs::File::open(part) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to open AI output log file {}: {}", part.display(), e);
                return CommandResponse {
                    success: false,
                    message: Some(format!("Failed to open log file: {}", e)),
                    data: None,
                };
            }
        };

        for line_result in BufReader::new(file).lines() {
            line_number += 1;
            match line_result {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<AiOutputEntry>(&line) {
                        Ok(entry) => entries.push(entry),
                        Err(e) => {
                            warn!(
                                "Failed to parse AI output entry at line {}: {}",
                                line_number, e
                            );
                            // Continue parsing other lines
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to read line {} from AI output log: {}",
                        line_number, e
                    );
                }
            }
        }
    }

    info!("Loaded {} AI output entries from log file", entries.len());

    CommandResponse {
        success: true,
        message: Some(format!("Loaded {} entries", entries.len())),
        data: Some(serde_json::json!({
            "entries": entries
        })),
    }
}

/// Get the base directory for dev logs
fn get_dev_logs_dir() -> PathBuf {
    crate::paths::get_dev_logs_dir()
}

/// List all session checkpoint files
/// Returns session IDs that have checkpoint files
#[tauri::command]
pub fn list_session_checkpoints() -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    if !dev_logs_dir.exists() {
        return CommandResponse {
            success: true,
            message: Some("No dev-logs directory".to_string()),
            data: Some(serde_json::json!({
                "session_ids": []
            })),
        };
    }

    let mut session_ids: Vec<String> = Vec::new();

    match fs::read_dir(&dev_logs_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                // Match pattern: session-{id}-checkpoint.json
                if name.starts_with("session-") && name.ends_with("-checkpoint.json") {
                    // Extract session ID
                    let id = name
                        .strip_prefix("session-")
                        .and_then(|s| s.strip_suffix("-checkpoint.json"))
                        .map(|s| s.to_string());
                    if let Some(id) = id {
                        session_ids.push(id);
                    }
                }
            }
        }
        Err(e) => {
            error!("Failed to read dev-logs directory: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to read directory: {}", e)),
                data: None,
            };
        }
    }

    info!("Found {} session checkpoint files", session_ids.len());

    CommandResponse {
        success: true,
        message: Some(format!("Found {} checkpoints", session_ids.len())),
        data: Some(serde_json::json!({
            "session_ids": session_ids
        })),
    }
}

/// Delete session checkpoint files by session IDs
/// Also cleans up session manager state file if all sessions are deleted
#[tauri::command]
pub fn delete_session_checkpoints(session_ids: Vec<String>) -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    if !dev_logs_dir.exists() {
        return CommandResponse {
            success: true,
            message: Some("No dev-logs directory".to_string()),
            data: Some(serde_json::json!({
                "deleted_count": 0
            })),
        };
    }

    let mut deleted_count = 0;
    let mut errors: Vec<String> = Vec::new();

    for session_id in &session_ids {
        let checkpoint_path = dev_logs_dir.join(format!("session-{}-checkpoint.json", session_id));

        if checkpoint_path.exists() {
            match fs::remove_file(&checkpoint_path) {
                Ok(_) => {
                    info!("Deleted checkpoint for session {}", session_id);
                    deleted_count += 1;
                }
                Err(e) => {
                    let err_msg = format!("Failed to delete checkpoint {}: {}", session_id, e);
                    error!("{}", err_msg);
                    errors.push(err_msg);
                }
            }
        }
    }

    // Also clean up session-manager-state.json if it exists and all sessions deleted
    let state_file_path = dev_logs_dir.join("session-manager-state.json");
    if state_file_path.exists() {
        // Try to remove it - it will be recreated if there are active sessions
        if let Err(e) = fs::remove_file(&state_file_path) {
            warn!("Failed to remove session manager state file: {}", e);
        } else {
            info!("Cleaned up session manager state file");
        }
    }

    if errors.is_empty() {
        CommandResponse {
            success: true,
            message: Some(format!("Deleted {} checkpoint(s)", deleted_count)),
            data: Some(serde_json::json!({
                "deleted_count": deleted_count
            })),
        }
    } else {
        CommandResponse {
            success: false,
            message: Some(format!(
                "Deleted {} checkpoint(s) with {} error(s): {}",
                deleted_count,
                errors.len(),
                errors.join("; ")
            )),
            data: Some(serde_json::json!({
                "deleted_count": deleted_count,
                "errors": errors
            })),
        }
    }
}

/// Delete all session checkpoints and AI output log
/// Complete cleanup of run history
#[tauri::command]
pub fn clear_all_run_history() -> CommandResponse {
    let dev_logs_dir = get_dev_logs_dir();

    let mut deleted_checkpoints = 0;
    let mut errors: Vec<String> = Vec::new();

    // Delete all session checkpoint files
    if dev_logs_dir.exists() {
        match fs::read_dir(&dev_logs_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let name = file_name.to_string_lossy();
                    // Match checkpoint files and state files
                    if (name.starts_with("session-") && name.ends_with("-checkpoint.json"))
                        || name == "session-manager-state.json"
                    {
                        if let Err(e) = fs::remove_file(entry.path()) {
                            errors.push(format!("Failed to delete {}: {}", name, e));
                        } else {
                            deleted_checkpoints += 1;
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read dev-logs directory: {}", e));
            }
        }
    }

    // Clear AI output log
    let log_path = get_ai_output_log_path();
    let log_deleted = if log_path.exists() {
        match fs::remove_file(&log_path) {
            Ok(_) => true,
            Err(e) => {
                errors.push(format!("Failed to delete AI output log: {}", e));
                false
            }
        }
    } else {
        false
    };

    info!(
        "Cleared run history: {} checkpoints, log deleted: {}",
        deleted_checkpoints, log_deleted
    );

    if errors.is_empty() {
        CommandResponse {
            success: true,
            message: Some(format!(
                "Cleared {} checkpoint(s) and AI output log",
                deleted_checkpoints
            )),
            data: Some(serde_json::json!({
                "deleted_checkpoints": deleted_checkpoints,
                "log_deleted": log_deleted
            })),
        }
    } else {
        CommandResponse {
            success: false,
            message: Some(format!(
                "Partial cleanup: {} checkpoint(s), errors: {}",
                deleted_checkpoints,
                errors.join("; ")
            )),
            data: Some(serde_json::json!({
                "deleted_checkpoints": deleted_checkpoints,
                "log_deleted": log_deleted,
                "errors": errors
            })),
        }
    }
}

// ============================================================================
// Render Logging - For UI Component Testing
// ============================================================================

/// Render log entry structure for component render logging.
/// Components log what data they are rendering so tests can verify display.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RenderLogEntry {
    /// Unique ID for this log entry
    pub id: String,
    /// Timestamp when the render occurred
    pub timestamp: i64,
    /// Component name (e.g., "RunRecapTab", "StatusBanner")
    pub component: String,
    /// What triggered this render (e.g., "mount", "data_update", "prop_change")
    pub trigger: String,
    /// The data that was rendered (component-specific structure)
    pub data: serde_json::Value,
    /// Optional: sections that are visible/expanded
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_sections: Option<Vec<String>>,
    /// Optional: task run ID if applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_run_id: Option<i64>,
    /// Window that produced this entry ("main" for the primary window; "term-N"
    /// for pop-out windows in Phase 1). Optional + skipped when None so existing
    /// entries and readers are unaffected; consumers treat a missing value as
    /// "main". See plan `2026-06-03-runner-popout-terminal-windows.md` Phase 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_label: Option<String>,
}

/// Get the path to the render log file
fn get_render_log_path() -> PathBuf {
    crate::paths::get_render_log_path()
}

/// Maximum render log file size before truncation (50 MB).
const RENDER_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Number of lines to keep when truncating the render log.
const RENDER_LOG_KEEP_LINES: usize = 200;

/// Append a render log entry to the log file.
/// Called by UI components when they render data.
/// Automatically truncates the file when it exceeds RENDER_LOG_MAX_BYTES.
#[tauri::command]
pub fn append_render_log(entry: RenderLogEntry) -> CommandResponse {
    let log_path = get_render_log_path();

    // Ensure directory exists
    if let Some(parent) = log_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            error!("Failed to create render log directory: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to create log directory: {}", e)),
                data: None,
            };
        }
    }

    // Check file size and truncate if too large
    if let Ok(metadata) = fs::metadata(&log_path) {
        if metadata.len() > RENDER_LOG_MAX_BYTES {
            info!(
                "Render log exceeds {} MB, truncating to last {} lines",
                RENDER_LOG_MAX_BYTES / (1024 * 1024),
                RENDER_LOG_KEEP_LINES
            );
            truncate_jsonl_file(&log_path, RENDER_LOG_KEEP_LINES);
        }
    }

    // Serialize to JSON
    let json_line = match serde_json::to_string(&entry) {
        Ok(json) => json,
        Err(e) => {
            error!("Failed to serialize render log entry: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to serialize: {}", e)),
                data: None,
            };
        }
    };

    // Append to file
    let mut file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to open render log file: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to open log file: {}", e)),
                data: None,
            };
        }
    };

    if let Err(e) = writeln!(file, "{}", json_line) {
        error!("Failed to write to render log: {}", e);
        return CommandResponse {
            success: false,
            message: Some(format!("Failed to write: {}", e)),
            data: None,
        };
    }

    CommandResponse {
        success: true,
        message: None,
        data: None,
    }
}

/// Truncate a JSONL file to keep only the last N lines.
fn truncate_jsonl_file(path: &std::path::Path, keep_lines: usize) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read file for truncation: {}", e);
            return;
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(keep_lines);
    let kept: String = lines[start..].iter().map(|l| format!("{}\n", l)).collect();

    if let Err(e) = fs::write(path, kept) {
        warn!("Failed to write truncated file: {}", e);
    } else {
        info!(
            "Truncated {} from {} to {} lines",
            path.display(),
            lines.len(),
            lines.len() - start
        );
    }
}

/// Clear the render log file.
/// Called at the start of a test run to ensure fresh logs.
#[tauri::command]
pub fn clear_render_log() -> CommandResponse {
    let log_path = get_render_log_path();

    if log_path.exists() {
        if let Err(e) = fs::remove_file(&log_path) {
            error!("Failed to clear render log: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to clear log: {}", e)),
                data: None,
            };
        }
        info!("Cleared render log");
    }

    CommandResponse {
        success: true,
        message: Some("Render log cleared".to_string()),
        data: None,
    }
}

/// Maximum number of render log entries to load at once.
const RENDER_LOG_LOAD_LIMIT: usize = 500;

/// Load render log entries from the log file (last N entries only).
/// Used by Python tests to verify component rendering.
#[tauri::command]
pub fn load_render_log() -> CommandResponse {
    let log_path = get_render_log_path();

    if !log_path.exists() {
        return CommandResponse {
            success: true,
            message: Some("No render log file exists".to_string()),
            data: Some(serde_json::json!({
                "entries": []
            })),
        };
    }

    let file = match fs::File::open(&log_path) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to open render log file: {}", e);
            return CommandResponse {
                success: false,
                message: Some(format!("Failed to open log file: {}", e)),
                data: None,
            };
        }
    };

    let reader = BufReader::new(file);
    // Use a ring buffer approach: keep only the last RENDER_LOG_LOAD_LIMIT entries
    let mut entries: std::collections::VecDeque<RenderLogEntry> =
        std::collections::VecDeque::with_capacity(RENDER_LOG_LOAD_LIMIT);
    let mut line_number = 0;
    let mut total_parsed = 0;

    for line_result in reader.lines() {
        line_number += 1;
        match line_result {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<RenderLogEntry>(&line) {
                    Ok(entry) => {
                        if entries.len() >= RENDER_LOG_LOAD_LIMIT {
                            entries.pop_front();
                        }
                        entries.push_back(entry);
                        total_parsed += 1;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse render log entry at line {}: {}",
                            line_number, e
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to read line {} from render log: {}", line_number, e);
            }
        }
    }

    let returned = entries.len();
    info!(
        "Loaded {} render log entries (total in file: {})",
        returned, total_parsed
    );

    let entries_vec: Vec<RenderLogEntry> = entries.into_iter().collect();

    CommandResponse {
        success: true,
        message: Some(format!(
            "Loaded {} entries (total: {})",
            returned, total_parsed
        )),
        data: Some(serde_json::json!({
            "entries": entries_vec
        })),
    }
}

/// Get the path to the render log file
#[tauri::command]
pub fn get_render_log_path_cmd() -> CommandResponse {
    let log_path = get_render_log_path();
    CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "path": log_path.to_string_lossy()
        })),
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_logging")
        .invoke_handler(tauri::generate_handler![
            append_ai_output_log,
            clear_ai_output_log,
            get_ai_output_log_path_cmd,
            load_ai_output_log,
            list_session_checkpoints,
            delete_session_checkpoints,
            clear_all_run_history,
            append_render_log,
            clear_render_log,
            load_render_log,
            get_render_log_path_cmd,
        ])
        .build()
}

#[cfg(test)]
mod ai_output_sink_tests {
    use super::*;

    fn lines_in(path: &std::path::Path) -> Vec<String> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn appends_lines_through_one_open_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let mut sink = AiOutputSink::new(path.clone());

        sink.write_line("{\"a\":1}");
        sink.write_line("{\"a\":2}");

        assert_eq!(lines_in(&path), vec!["{\"a\":1}", "{\"a\":2}"]);
    }

    #[test]
    fn rotation_moves_the_file_aside_instead_of_rewriting_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let mut sink = AiOutputSink::new(path.clone());

        // Fill past the cap using the real threshold accounting, then force the
        // rotation by pretending the file is already at the limit.
        sink.write_line("first");
        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("second");

        let sidecar = sink.sidecar();
        assert_eq!(
            sidecar.file_name().unwrap().to_string_lossy(),
            "ai-output.jsonl.1"
        );
        // Pre-rotation content is preserved in the sidecar — nothing is read
        // back and rewritten, so no line can be lost to an interleaved append.
        assert_eq!(lines_in(&sidecar), vec!["first"]);
        // The live file restarts from the line that triggered the rotation.
        assert_eq!(lines_in(&path), vec!["second"]);
    }

    #[test]
    fn second_rotation_replaces_the_previous_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let mut sink = AiOutputSink::new(path.clone());

        sink.write_line("gen1");
        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("gen2");
        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("gen3");

        assert_eq!(lines_in(&sink.sidecar()), vec!["gen2"]);
        assert_eq!(lines_in(&path), vec!["gen3"]);
    }

    #[test]
    fn clear_removes_both_generations_and_the_sink_recovers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let mut sink = AiOutputSink::new(path.clone());

        sink.write_line("old");
        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("newer");
        let sidecar = sink.sidecar();
        assert!(sidecar.exists());

        sink.clear();
        assert!(!path.exists());
        assert!(!sidecar.exists());

        // The next line reopens a fresh file rather than wedging.
        sink.write_line("after-clear");
        assert_eq!(lines_in(&path), vec!["after-clear"]);
    }

    /// REGRESSION (PR #961 review F2). Rotation moves history into the sidecar;
    /// every reader must consume the sidecar BEFORE the live file, or a
    /// rotation orphans the older generation from the AI Output viewer and
    /// `consolidate_ai_output`.
    #[test]
    fn rotated_read_set_is_sidecar_then_live_in_that_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");

        // Nothing on disk yet — an empty set, not a phantom path.
        assert!(rotated_log_read_set(&path).is_empty());

        let mut sink = AiOutputSink::new(path.clone());
        sink.write_line("older");
        // Only the live file exists.
        assert_eq!(rotated_log_read_set(&path), vec![path.clone()]);

        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("newer");

        let sidecar = rotation_sidecar_path(&path);
        assert_eq!(
            rotated_log_read_set(&path),
            vec![sidecar.clone(), path.clone()],
            "the older generation must be read first"
        );
        // Concatenated in set order, the stream is chronological again.
        let combined: Vec<String> = rotated_log_read_set(&path)
            .iter()
            .flat_map(|p| lines_in(p))
            .collect();
        assert_eq!(combined, vec!["older", "newer"]);
    }

    /// REGRESSION (PR #961 review F4). The direct-append fallback runs when the
    /// writer thread is missing or dead — i.e. when nothing else can rotate —
    /// so it must enforce the cap itself instead of growing without bound.
    #[test]
    fn direct_append_fallback_rotates_at_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let sidecar = rotation_sidecar_path(&path);
        // Tiny cap so the branch is reachable without writing 50 MB.
        let cap = 12u64;

        // Each append writes the line plus a newline: 5 bytes, then 5 more.
        append_line_direct_at(&path, "aaaa", cap).expect("first append");
        append_line_direct_at(&path, "bbbb", cap).expect("second append");
        assert!(
            !sidecar.exists(),
            "10 bytes is under the 12-byte cap — nothing to rotate yet"
        );

        // 10 + 6 > 12 → rotate, then append into the fresh file.
        append_line_direct_at(&path, "ccccc", cap).expect("rotating append");
        assert_eq!(lines_in(&sidecar), vec!["aaaa", "bbbb"]);
        assert_eq!(lines_in(&path), vec!["ccccc"]);
        // Both generations remain readable, oldest first.
        let combined: Vec<String> = rotated_log_read_set(&path)
            .iter()
            .flat_map(|p| lines_in(p))
            .collect();
        assert_eq!(combined, vec!["aaaa", "bbbb", "ccccc"]);
    }

    /// REGRESSION (PR #961 review F4). A fallback clear must remove the sidecar
    /// too — otherwise "cleared" still serves the previous run's output,
    /// because readers concatenate the sidecar.
    #[test]
    fn fallback_clear_removes_both_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");
        let mut sink = AiOutputSink::new(path.clone());
        sink.write_line("old");
        sink.written = AI_OUTPUT_LOG_MAX_BYTES;
        sink.write_line("newer");
        drop(sink); // release the handle so Windows can delete
        let sidecar = rotation_sidecar_path(&path);
        assert!(sidecar.exists() && path.exists());

        clear_ai_output_files_at(&path).expect("fallback clear");
        assert!(!path.exists());
        assert!(!sidecar.exists());
        assert!(rotated_log_read_set(&path).is_empty());
    }

    /// REGRESSION (PR #961 review F3). The writer thread's sender is a
    /// process-lifetime static, so `recv` never returns on its own — a Flush
    /// barrier is the only way to know queued lines have landed. This drives
    /// the worker exactly as `flush_ai_output_log` does.
    #[test]
    fn flush_barrier_acks_only_after_queued_lines_are_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ai-output.jsonl");

        let (tx, rx) = std::sync::mpsc::sync_channel::<AiLogMsg>(AI_OUTPUT_LOG_QUEUE_CAP);
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            // Same loop body as `ai_output_log_worker`, against a temp path.
            let mut sink = AiOutputSink::new(worker_path);
            while let Ok(msg) = rx.recv() {
                match msg {
                    AiLogMsg::Line(line) => sink.write_line(&line),
                    AiLogMsg::Clear => sink.clear(),
                    AiLogMsg::Flush(ack) => {
                        sink.sync();
                        let _ = ack.send(());
                    }
                }
            }
        });

        for i in 0..500 {
            tx.send(AiLogMsg::Line(format!("line-{}", i)))
                .expect("queue");
        }

        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel::<()>(1);
        tx.send(AiLogMsg::Flush(ack_tx)).expect("queue flush");
        ack_rx
            .recv_timeout(AI_OUTPUT_LOG_FLUSH_TIMEOUT)
            .expect("flush must be acknowledged");

        // The ack is a barrier: every queued line is on disk BEFORE it returns.
        let lines = lines_in(&path);
        assert_eq!(lines.len(), 500);
        assert_eq!(lines[0], "line-0");
        assert_eq!(lines[499], "line-499");

        drop(tx);
        worker.join().expect("worker exits when the sender drops");
    }
}
