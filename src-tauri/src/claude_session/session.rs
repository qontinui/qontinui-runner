//! ClaudeSession - interactive bidirectional session with Claude CLI.
//!
//! Manages the lifecycle of a Claude CLI process using the stream-json protocol.
//! Supports multiple turns, user message queuing, and interrupt.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::Emitter;
use tracing::{debug, error, info, warn};

// `RunnerObservableBridge` is in scope so `bridge.pull` / `bridge.reconcile`
// resolve through the trait — `MemoryBridge`'s impl is the only one
// today and the receiver `Arc<MemoryBridge>` requires the trait.
use qontinui_runner_lib::observable_bridge::RunnerObservableBridge;

use crate::claude_protocol::request_id::next_request_id;
use crate::claude_protocol::types::{OutgoingControlRequest, UserInputMessage};
use crate::findings::{
    Finding, FindingCategoryExt, FindingParser, FindingSeverityExt, ParsedFinding,
};
use crate::mcp::shared::{emit_ai_output, AiSessionContext, FindingContext, ProgressContext};
use crate::workflow_state::{ParsedProgress, ProgressParser};

use super::dispatcher;
use super::state::{SessionState, SessionStateTracker};
use super::writer::StdinWriter;

#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;

/// Maximum number of pending user messages in the queue.
const MAX_PENDING_MESSAGES: usize = 10;

/// Metadata about a git worktree this session has been promoted into.
///
/// When `Some`, the underlying Claude CLI process has its `cwd` set to
/// `path` and any file work it does is isolated to the `branch` checkout.
/// Phase 3 (dispatcher.rs) reads `id` to scope the file registry so two
/// concurrent sessions writing to the same logical file but in different
/// worktrees do not flag each other as conflicts.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Worktrees table primary key (matches the `worktrees.id` TEXT column).
    pub id: String,
    /// Absolute path to the worktree checkout directory (also the CLI cwd).
    pub path: PathBuf,
    /// Git branch name created for this worktree.
    pub branch: String,
}

/// Internal error type for `build_replay_from_history`.
///
/// Distinguishes "no history found" (`Ok(None)`) from "PG isn't available"
/// (`Err(NoPg)`) so that callers like `promote_to_worktree` can decide
/// whether to abort or proceed without replay context.
#[derive(Debug)]
enum BuildReplayErr {
    /// PG global isn't initialized — no way to read the output_log.
    NoPg,
}

/// An interactive Claude CLI session.
pub struct ClaudeSession {
    /// Session identifier (typically the task_run_id + phase suffix).
    session_id: String,
    /// Friendly display name for this session. Mirrors what the file-lock
    /// dispatcher (`claude_session/dispatcher.rs:398-404`) emits as
    /// `holder_name` on `file-lock-*` events:
    /// `session_ctx.session_name` when present (workflow path), else falls
    /// back to `session_id` (terminal path). Stored here so out-of-process
    /// consumers (e.g. `GET /sessions/idle-status`) can surface the same
    /// label without re-deriving it.
    holder_name: String,
    /// State machine for session lifecycle.
    state_tracker: SessionStateTracker,
    /// Thread-safe stdin writer.
    stdin_writer: Arc<StdinWriter>,
    /// Queue of user messages waiting to be sent (when session is Processing).
    pending_messages: Arc<Mutex<VecDeque<String>>>,
    /// Accumulated text output from the session.
    accumulated_output: Arc<Mutex<String>>,
    /// Whether a user has sent at least one message (for autonomous/interactive switching).
    user_has_interacted: Arc<AtomicBool>,
    /// Child process ID (for stop/kill).
    child_pid: u32,
    /// Last activity timestamp (for timeout monitoring).
    last_activity: Arc<AtomicU64>,
    /// Whether the session has produced output.
    has_output: Arc<AtomicBool>,
    /// Sender to signal the heartbeat thread to stop.
    stop_heartbeat: Option<mpsc::Sender<()>>,
    /// Handle to the stdout reader thread.
    stdout_join: Mutex<Option<thread::JoinHandle<String>>>,
    /// Handle to the stderr reader thread.
    stderr_join: Mutex<Option<thread::JoinHandle<String>>>,
    /// Handle to the heartbeat thread.
    heartbeat_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Handle to the finding processor thread.
    finding_join: Mutex<Option<thread::JoinHandle<Vec<Finding>>>>,
    /// Handle to the progress processor thread.
    progress_join: Mutex<Option<thread::JoinHandle<u32>>>,
    /// Shared output buffer (for timeout recovery).
    shared_output_buf: Arc<Mutex<String>>,
    /// Raw stdout pipe handle (for Windows pipe close workaround).
    #[cfg(target_os = "windows")]
    stdout_raw_handle: Option<std::os::windows::io::RawHandle>,
    /// PID tracker for external stop support.
    pid_tracker: Option<Arc<Mutex<Vec<u32>>>>,
    /// Sender to signal completion to wait_for_result().
    completion_tx: Mutex<Option<mpsc::Sender<(bool, String)>>>,
    /// Tracks how many bytes of accumulated_output have been persisted to DB.
    persisted_output_len: Arc<AtomicUsize>,
    /// Sender for AI output deltas to persist to DB after each turn.
    turn_persist_tx: Option<mpsc::Sender<String>>,
    /// Worktree metadata when this session has been promoted into an isolated
    /// git worktree. `None` means the session is running in the original
    /// working directory (the default for fresh sessions).
    worktree: Option<WorktreeInfo>,
    /// Memory-federation context for this session, when federation is
    /// enabled and identity could be resolved at spawn time. The waiter
    /// thread (see `wait_for_child` callsite) reads it on subprocess
    /// exit to fire the session-end reconcile + Tauri event broadcast.
    /// `None` means federation is disabled or unconfigured — the session
    /// runs purely against the local memory dir.
    ///
    /// Plan: `2026-05-22-memories-on-coord-cross-machine.md` Phase 5.D.
    federation_ctx: Option<qontinui_runner_lib::observable_bridge::SessionContext>,
}

// SAFETY: ClaudeSession contains a raw Windows handle (RawHandle = *mut c_void) for the stdout
// pipe close workaround. This handle is only used in close() to call CloseHandle(), which is
// thread-safe. All other fields are already Send+Sync (Arc<Mutex<...>>, atomics, mpsc senders).
unsafe impl Send for ClaudeSession {}
unsafe impl Sync for ClaudeSession {}

impl ClaudeSession {
    /// Spawn a new interactive Claude CLI session.
    ///
    /// Performs the initialization handshake (sends `initialize` control request,
    /// waits for control response) before returning.
    ///
    /// When `worktree` is `Some`, `working_dir` MUST already be the worktree's
    /// path — the CLI inherits its cwd from there and the metadata is stored on
    /// the resulting session for downstream consumers (e.g. file registry).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        working_dir: &str,
        session_id: &str,
        app_handle: &tauri::AppHandle,
        session_ctx: Option<AiSessionContext>,
        finding_ctx: Option<FindingContext>,
        progress_ctx: Option<ProgressContext>,
        pid_tracker: Option<Arc<Mutex<Vec<u32>>>>,
        model_override: Option<&str>,
        worktree: Option<WorktreeInfo>,
        tool_policy: Option<&crate::workflow::dag_schema::ToolPolicy>,
    ) -> Result<Self, String> {
        info!(
            "Spawning interactive Claude session: {} in {}",
            session_id, working_dir
        );

        // Spawn CLI with stream-json input AND output
        let mut cmd = crate::process_helpers::cmd_no_window();
        let mut cli_args = vec![
            "/c".to_string(),
            "claude".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
        ];

        // Add model override if specified (e.g., from per-stage config)
        if let Some(model) = model_override {
            cli_args.push("--model".to_string());
            cli_args.push(model.to_string());
            info!(
                "Using model override for interactive session {}: {}",
                session_id, model
            );
        }

        // ── Blueprint tool policy (Phase 3) ───────────────────────────────
        // Same carrier as the inline runner: allow/deny flags plus, for
        // argument-scoped denies, an ephemeral --settings <file> JSON block.
        // None ⇒ no-op (no extra args, no settings file), preserving today's
        // interactive spawn byte-for-byte.
        let tool_policy_settings_path: Option<std::path::PathBuf>;
        if let Some(policy) = tool_policy {
            let (extra_args, settings_json) =
                crate::claude_session::tool_policy_args::build_tool_policy_cli(policy);
            cli_args.extend(extra_args);
            tool_policy_settings_path = if let Some(json) = settings_json {
                let settings_file = std::env::temp_dir()
                    .join(format!("claude_session_settings_{}.json", session_id));
                std::fs::write(&settings_file, &json)
                    .map_err(|e| format!("Failed to write tool-policy settings file: {}", e))?;
                cli_args.push("--settings".to_string());
                cli_args.push(settings_file.to_string_lossy().to_string());
                info!(
                    "Applied tool-policy settings for interactive session {} at {}",
                    session_id,
                    settings_file.display()
                );
                Some(settings_file)
            } else {
                None
            };
        } else {
            tool_policy_settings_path = None;
        }
        // The settings file is consumed by the spawned CLI at startup; it is a
        // best-effort temp artifact left in temp_dir (mirrors the inline runner
        // when the path can't be tracked to session end). Bind to silence the
        // unused warning when no policy is present.
        let _ = &tool_policy_settings_path;

        let cli_arg_refs: Vec<&str> = cli_args.iter().map(|s| s.as_str()).collect();
        cmd.args(&cli_arg_refs)
            .current_dir(working_dir)
            // Remove CLAUDECODE env var to prevent "nested session" detection.
            // The runner spawns Claude CLI as an automation tool, not as a nested session.
            .env_remove("CLAUDECODE")
            .env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set CLAUDE_CONFIG_DIR to the resolved account (for multi-account support)
        let ai_settings = crate::settings::get_ai_settings();
        let effective_config_dir =
            crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
        // Silently refresh OAuth credentials if expired before spawning the subprocess.
        crate::ai_provider::oauth_refresh::try_ensure_valid_credentials(
            effective_config_dir.as_deref(),
        );
        if let Some(ref config_dir) = effective_config_dir {
            info!(
                "Setting CLAUDE_CONFIG_DIR={} for session {}",
                config_dir, session_id
            );
            cmd.env("CLAUDE_CONFIG_DIR", config_dir.as_str());
        }

        // ── Memory federation: spawn-time pull + start watcher ────────
        //
        // Plan 2026-05-22-memories-on-coord-cross-machine.md Phase 5.D.
        // The materialization MUST land before `cmd.spawn()` because
        // Claude's auto-memory subsystem reads `memory_dir` during init.
        // The watcher starts immediately after pull so in-session writes
        // get pushed to coord without waiting for session-end. All
        // identity / toggle short-circuits return `None` so federation
        // never blocks the spawn.
        let federation_ctx = if crate::claude_session::federation::federation_enabled() {
            match super::federation::build_federation_ctx(
                session_id,
                working_dir,
                &ai_settings.claude_cli,
            ) {
                Ok(ctx) => Some(ctx),
                Err(reason) => {
                    // Surface the skip reason to the UI (warn! already
                    // logged inside build_federation_ctx); proceed locally.
                    super::federation::emit_federation_skip(
                        app_handle,
                        session_id,
                        reason.as_str(),
                        reason.detail(),
                    );
                    None
                }
            }
        } else {
            debug!(
                "memory federation disabled via settings; skipping pull/watch for session {}",
                session_id
            );
            super::federation::emit_federation_skip(
                app_handle,
                session_id,
                "disabled",
                "Memory federation disabled via settings kill-switch.",
            );
            None
        };
        let federation_ctx = if let Some(mut ctx) = federation_ctx {
            // Iterate every registered observable bridge: a per-bridge
            // pull failure skips that bridge's watcher but never kills the
            // session or the other bridges.
            Self::block_on_async(async {
                for b in qontinui_runner_lib::observable_bridge::global_registry() {
                    match b.pull(&mut ctx).await {
                        Ok(()) => {
                            if let Err(e) = std::sync::Arc::clone(b).start_watching(&ctx).await {
                                warn!(
                                    "federation[{}]: start_watching failed for session {} ({}); \
                                     continuing without watcher — reconcile will still run",
                                    b.category(),
                                    session_id,
                                    e
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                "federation[{}]: pull failed for session {} ({}); \
                                 skipping watcher for this bridge",
                                b.category(),
                                session_id,
                                e
                            );
                        }
                    }
                }
            });
            Some(ctx)
        } else {
            None
        };

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // Clean up every federation watcher we just started so a
                // spawn failure doesn't leave an orphaned `notify` hook
                // running for the lifetime of the runner process.
                if let Some(ctx) = federation_ctx.as_ref() {
                    Self::block_on_async(async {
                        for b in qontinui_runner_lib::observable_bridge::global_registry() {
                            b.stop_watching(ctx.session_id).await;
                        }
                    });
                }
                return Err(format!("Failed to spawn Claude CLI: {}", e));
            }
        };

        // Assign to Windows Job Object for crash safety (auto-kill on runner exit)
        #[cfg(target_os = "windows")]
        crate::job_object::assign_process_to_job(
            child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
        );

        let child_pid = child.id();
        info!("Claude CLI spawned with PID {}", child_pid);

        // Register PID for external stop support
        if let Some(ref tracker) = pid_tracker {
            if let Ok(mut pids) = tracker.lock() {
                pids.push(child_pid);
                info!("Registered AI process PID {}", child_pid);
            }
        }

        // Take stdin, stdout, stderr
        let stdin = child
            .stdin
            .take()
            .ok_or("Failed to take stdin from Claude CLI")?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Save raw stdout handle for Windows pipe close workaround
        #[cfg(target_os = "windows")]
        let stdout_raw_handle = stdout.as_ref().map(|s| s.as_raw_handle());

        // Create shared state
        let state_tracker = SessionStateTracker::new();
        let stdin_writer = Arc::new(StdinWriter::new(stdin));
        let pending_messages: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let accumulated_output = Arc::new(Mutex::new(String::new()));
        let user_has_interacted = Arc::new(AtomicBool::new(false));
        let shared_output_buf = Arc::new(Mutex::new(String::new()));

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_activity = Arc::new(AtomicU64::new(now_secs));
        let has_output = Arc::new(AtomicBool::new(false));

        // Transition to Initializing
        state_tracker
            .transition(SessionState::Initializing)
            .map_err(|e| format!("State transition failed: {}", e))?;

        // Send initialize control request
        let init_request_id = next_request_id();
        let init_msg = OutgoingControlRequest::initialize(&init_request_id);
        stdin_writer
            .write_message(&init_msg)
            .map_err(|e| format!("Failed to send init request: {}", e))?;
        info!("Sent initialize request (id: {})", init_request_id);

        // AI output persistence tracking
        let persisted_output_len = Arc::new(AtomicUsize::new(0));

        // Create a channel for persisting AI output to DB after each turn.
        // The persister thread receives output deltas and writes them with
        // [AI_RESPONSE] markers via append_task_output_ex.
        let (turn_persist_tx, turn_persist_rx) = mpsc::channel::<String>();
        let turn_persist_tx_option = if session_ctx.is_some() {
            let persist_task_run_id = session_ctx
                .as_ref()
                .map(|c| c.task_run_id().to_string())
                .unwrap_or_default();

            thread::spawn(move || {
                let pg = crate::database::pg::PgDb::try_global();

                let pg = match pg {
                    Some(pg) => pg,
                    None => {
                        warn!("Failed to get PG connection for AI output persistence");
                        // Drain the channel so senders don't block
                        while turn_persist_rx.recv().is_ok() {}
                        return;
                    }
                };

                while let Ok(delta) = turn_persist_rx.recv() {
                    if delta.is_empty() {
                        continue;
                    }
                    let formatted = format!("\n[AI_RESPONSE]\n{}\n[/AI_RESPONSE]\n", delta);
                    if let Err(e) = tauri::async_runtime::block_on(pg.append_task_output_ex(
                        &persist_task_run_id,
                        &formatted,
                        false,
                        false,
                    )) {
                        warn!("Failed to persist AI response to output_log: {}", e);
                    } else {
                        debug!(
                            "Persisted AI response ({} chars) for task_run_id={}",
                            delta.len(),
                            persist_task_run_id
                        );
                    }
                }
                debug!("AI output persister thread exiting");
            });

            Some(turn_persist_tx)
        } else {
            // No session context — drop the receiver so channel is closed
            drop(turn_persist_rx);
            None
        };

        // Channels for findings and progress
        let (finding_tx, finding_rx) = mpsc::channel::<ParsedFinding>();
        let (progress_tx, progress_rx) = mpsc::channel::<ParsedProgress>();

        // Heartbeat thread
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let app_handle_heartbeat = app_handle.clone();
        let session_ctx_heartbeat = session_ctx.clone();
        let has_output_heartbeat = has_output.clone();
        let start_time = Instant::now();

        let heartbeat_handle = thread::spawn(move || {
            let mut last_update = 0u64;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));

                if has_output_heartbeat.load(Ordering::Relaxed) {
                    continue;
                }

                let elapsed_secs = start_time.elapsed().as_secs();
                if elapsed_secs > 0
                    && elapsed_secs.is_multiple_of(30)
                    && elapsed_secs != last_update
                {
                    last_update = elapsed_secs;
                    let mins = elapsed_secs / 60;
                    let secs = elapsed_secs % 60;
                    let msg = if mins > 0 {
                        format!("⏳ AI is working... ({}m {}s)", mins, secs)
                    } else {
                        format!("⏳ AI is working... ({}s)", secs)
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(
                            &app_handle_heartbeat,
                            &msg,
                            "status",
                            None,
                            session_ctx_heartbeat.as_ref(),
                        );
                    }));
                }
            }
        });

        // Stdout reader thread
        let state_for_stdout = state_tracker.clone();
        let writer_for_stdout = stdin_writer.clone();
        let pending_for_stdout = pending_messages.clone();
        let accumulated_for_stdout = accumulated_output.clone();
        let user_interacted_for_stdout = user_has_interacted.clone();
        let app_handle_stdout = app_handle.clone();
        let session_ctx_stdout = session_ctx.clone();
        let has_output_stdout = has_output.clone();
        let last_activity_stdout = last_activity.clone();
        let shared_output_for_thread = shared_output_buf.clone();
        let finding_ctx_for_stdout = finding_ctx.clone();
        let progress_ctx_for_stdout = progress_ctx.clone();
        let persist_tx_for_stdout = turn_persist_tx_option.clone();
        let persisted_len_for_stdout = persisted_output_len.clone();
        // Worktree ID for file-registry scoping (None for unpromoted sessions).
        // Cloned here so the stdout reader thread can move its own copy.
        let worktree_id_for_stdout: Option<String> = worktree.as_ref().map(|w| w.id.clone());
        // Fallback session ID for file locking when session_ctx is None (terminal sessions)
        let fallback_id_for_stdout = if session_ctx.is_none() {
            Some(session_id.to_string())
        } else {
            None
        };

        let stdout_handle = thread::spawn(move || {
            let mut all_text = String::new();
            let mut line_buffer = String::new();

            let mut finding_parser = if finding_ctx_for_stdout.is_some() {
                Some(FindingParser::new())
            } else {
                None
            };
            let mut progress_parser = if progress_ctx_for_stdout.is_some() {
                Some(ProgressParser::new())
            } else {
                None
            };

            if let Some(stdout) = stdout {
                info!("[STDOUT_READER] Thread started, reading lines from Claude CLI stdout");
                let reader = BufReader::new(stdout);
                let mut line_count = 0u64;
                for result in reader.lines() {
                    match result {
                        Ok(line) => {
                            line_count += 1;
                            let preview = crate::claude_protocol::codec::truncate_str(&line, 150);
                            info!("[STDOUT_READER] Line #{}: {}", line_count, preview);

                            // Update activity
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            last_activity_stdout.store(now, Ordering::Relaxed);

                            if let Some(text) = dispatcher::dispatch_line(
                                &line,
                                &app_handle_stdout,
                                session_ctx_stdout.as_ref(),
                                finding_parser.as_mut(),
                                progress_parser.as_mut(),
                                &finding_tx,
                                &progress_tx,
                                &mut line_buffer,
                                &state_for_stdout,
                                &writer_for_stdout,
                                &pending_for_stdout,
                                &accumulated_for_stdout,
                                &user_interacted_for_stdout,
                                &persist_tx_for_stdout,
                                &persisted_len_for_stdout,
                                fallback_id_for_stdout.as_deref(),
                                worktree_id_for_stdout.as_deref(),
                            ) {
                                has_output_stdout.store(true, Ordering::Relaxed);
                                all_text.push_str(&text);

                                // Sync to shared buffer periodically
                                if let Ok(mut buf) = shared_output_for_thread.lock() {
                                    *buf = all_text.clone();
                                }
                                // When DB persistence is active, clear local buffer after
                                // sync to prevent unbounded memory growth. The shared_output_buf
                                // has the latest snapshot, and the full history is persisted
                                // to DB via turn_persist_tx.
                                if persist_tx_for_stdout.is_some() {
                                    all_text.clear();
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "[STDOUT_READER] Error reading line after {} lines: {}",
                                line_count, e
                            );
                            break;
                        }
                    }
                }
                info!(
                    "[STDOUT_READER] Loop ended after {} lines (EOF or error)",
                    line_count
                );
            } else {
                warn!("[STDOUT_READER] No stdout handle available!");
            }

            // Flush remaining line buffer
            dispatcher::flush_line_buffer(
                &line_buffer,
                finding_parser.as_mut(),
                progress_parser.as_mut(),
                &finding_tx,
                &progress_tx,
            );

            // Final sync — only overwrite shared_output_buf if all_text still
            // has content. When DB persistence is active, all_text is cleared
            // after each sync (to prevent OOM), so the shared buffer already
            // holds the last valid snapshot and must not be overwritten with empty.
            if !all_text.is_empty() {
                if let Ok(mut buf) = shared_output_for_thread.lock() {
                    *buf = all_text.clone();
                }
            }

            // Release file locks held by this terminal session
            if let Some(ref fallback_id) = fallback_id_for_stdout {
                use crate::commands::AppState;
                use tauri::Manager;
                if let Some(app_state) = app_handle_stdout.try_state::<std::sync::Arc<AppState>>() {
                    let released_paths = app_state.file_lock_manager.release_all_sync(fallback_id);
                    // Emit file-lock-released for each path so frontends
                    // can clear blocked-on indicators without waiting for
                    // the next /file-locks/info poll. Mirrors the
                    // file-lock-acquired emit in
                    // claude_session/dispatcher.rs.
                    for released_path in &released_paths {
                        let payload = serde_json::json!({
                            "type": "file-lock-released",
                            "file_path": released_path,
                            "task_run_id": fallback_id,
                            "holder_name": fallback_id,
                        });
                        let _ = app_handle_stdout.emit("file-lock-released", &payload);
                    }
                    app_state
                        .file_registry_manager
                        .release_all_sync(fallback_id);
                    info!(
                        "[STDOUT_READER] Released file locks for terminal session {}",
                        fallback_id
                    );
                }
            }

            all_text
        });

        // Finding processor thread (same pattern as existing code)
        let app_handle_findings = app_handle.clone();
        let finding_ctx_for_processor = finding_ctx.clone();
        let session_ctx_for_findings = session_ctx.clone();

        // Capture the tokio runtime handle here — `thread::spawn` creates a
        // bare OS thread with no runtime context, so `Handle::current()`
        // inside the closure would panic with "there is no reactor running"
        // (observed 2026-04-23 under load). Cloning an existing Handle is
        // cheap and lets us call `handle.block_on(..)` from the sync thread.
        let finding_rt_handle = tokio::runtime::Handle::try_current().ok();

        let finding_handle = thread::spawn(move || {
            let mut detected_findings: Vec<Finding> = Vec::new();

            if let Some(ctx) = finding_ctx_for_processor {
                let pg_available = crate::database::pg::PgDb::try_global().is_some()
                    && finding_rt_handle.is_some();

                while let Ok(parsed_finding) = finding_rx.recv() {
                    info!(
                        "Detected finding: {} ({}:{})",
                        parsed_finding.title,
                        parsed_finding.category.as_str(),
                        parsed_finding.severity.as_str()
                    );

                    if pg_available {
                        let is_resolved = parsed_finding.is_resolved;

                        // Use the captured handle's `block_on` — `block_in_place`
                        // cannot be used here because it itself requires being
                        // inside a multi-threaded runtime, which this std::thread
                        // is not.
                        let pg = crate::database::pg::PgDb::global();
                        let rt = finding_rt_handle
                            .as_ref()
                            .expect("pg_available gated on finding_rt_handle.is_some()");
                        let insert_result = rt.block_on(async {
                            pg.insert_parsed_finding(
                                &ctx.task_run_id,
                                ctx.session_num,
                                &parsed_finding,
                            )
                            .await
                        });
                        match insert_result {
                            Ok(finding) => {
                                let event_name = if is_resolved {
                                    "finding_resolved"
                                } else {
                                    "finding_detected"
                                };

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        if let Err(e) =
                                            app_handle_findings.emit(event_name, &finding)
                                        {
                                            warn!("Failed to emit {} event: {}", event_name, e);
                                        }
                                        // Also broadcast to WebSocket clients
                                        if let Ok(json) = serde_json::to_value(&finding) {
                                            crate::event_system::broadcast_ws_notification(
                                                &app_handle_findings,
                                                event_name,
                                                &json,
                                            );
                                        }
                                    }));

                                let finding_msg = if is_resolved {
                                    format!(
                                        "Finding resolved: [{}:{}] {}",
                                        finding.category.as_str(),
                                        finding.severity.as_str(),
                                        finding.title
                                    )
                                } else {
                                    format!(
                                        "Finding detected: [{}:{}] {}",
                                        finding.category.as_str(),
                                        finding.severity.as_str(),
                                        finding.title
                                    )
                                };
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_findings,
                                            &finding_msg,
                                            "finding",
                                            None,
                                            session_ctx_for_findings.as_ref(),
                                        );
                                    }));

                                // Async-enrich security findings with vulnerability intelligence.
                                // Must use the captured runtime handle — bare `tokio::spawn`
                                // calls `Handle::current()` internally and would panic on this
                                // sync std::thread (same reactor-missing class as 46a426c1f).
                                if parsed_finding.category
                                    == crate::findings::FindingCategory::Security
                                {
                                    let description = finding.description.clone();
                                    let task_run_id = ctx.task_run_id.clone();
                                    let finding_title = finding.title.clone();
                                    rt.spawn(async move {
                                        let ka =
                                            crate::knowledge_acquisition::KnowledgeAcquisition::new(
                                            );
                                        if let Some(data) = crate::knowledge_acquisition::vuln_enrichment::enrich_from_description(&description, &ka).await {
                                            tracing::info!(
                                                "[knowledge_acquisition] Enriched security finding '{}': {} CVEs, exploit_available={}",
                                                finding_title,
                                                data.cve_ids.len(),
                                                data.exploit_available
                                            );
                                            // Store enrichment as task_knowledge via PG
                                            if let Some(pg) = crate::database::pg::PgDb::try_global() {
                                                let content = format!(
                                                    "## Vulnerability Enrichment: {}\n\n{}",
                                                    finding_title,
                                                    data.to_markdown()
                                                );
                                                let knowledge_id = uuid::Uuid::new_v4().to_string();
                                                if let Err(e) = pg.create_task_knowledge(
                                                    &knowledge_id,
                                                    &task_run_id,
                                                    "vulnerability_enrichment",
                                                    "system",
                                                    0,
                                                    &content,
                                                    None,
                                                    "high",
                                                    "[]",
                                                ).await {
                                                    tracing::warn!("[knowledge_acquisition] Failed to store enrichment: {}", e);
                                                } else {
                                                    // Best-effort embed so the row is searchable via Tier 0.
                                                    let _ = pg
                                                        .embed_knowledge_content(&knowledge_id, &content)
                                                        .await;
                                                }
                                            }
                                        }
                                    });
                                }

                                detected_findings.push(finding);
                            }
                            Err(e) => {
                                warn!("Failed to store finding: {}", e);
                            }
                        }
                    }
                }
            } else {
                while finding_rx.recv().is_ok() {}
            }

            detected_findings
        });

        // Progress processor thread (same pattern as existing code)
        let app_handle_progress = app_handle.clone();
        let progress_ctx_for_processor = progress_ctx.clone();
        let session_ctx_for_progress = session_ctx.clone();

        let progress_handle = thread::spawn(move || {
            let mut progress_count: u32 = 0;

            if let Some(ctx) = progress_ctx_for_processor {
                // Use PgDb for progress storage (async via tauri's runtime)
                let pg = crate::database::pg::PgDb::try_global();

                while let Ok(parsed_progress) = progress_rx.recv() {
                    debug!(
                        "Detected progress: {} - {}/{}",
                        parsed_progress.marker_type,
                        parsed_progress.current,
                        parsed_progress
                            .total
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    );

                    if let Some(ref pg) = pg {
                        let is_step_complete = parsed_progress.marker_type
                            == crate::workflow_state::progress_markers::STEP_COMPLETE;
                        let data_json = if is_step_complete {
                            parsed_progress
                                .sub_step_id
                                .as_ref()
                                .map(|id| serde_json::json!({ "sub_step_id": id }).to_string())
                        } else {
                            None
                        };

                        match tauri::async_runtime::block_on(pg.save_step_progress_marker(
                            &ctx.checkpoint_id,
                            &parsed_progress.marker_type,
                            parsed_progress.current,
                            parsed_progress.total,
                            parsed_progress.description.as_deref(),
                            data_json.as_deref(),
                        )) {
                            Ok(_) => {
                                progress_count += 1;

                                let progress_event = serde_json::json!({
                                    "checkpoint_id": ctx.checkpoint_id,
                                    "task_run_id": ctx.task_run_id,
                                    "marker_type": parsed_progress.marker_type,
                                    "current": parsed_progress.current,
                                    "total": parsed_progress.total,
                                    "percentage": parsed_progress.percentage(),
                                    "description": parsed_progress.description,
                                });

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        if let Err(e) = app_handle_progress
                                            .emit("step_progress", &progress_event)
                                        {
                                            warn!("Failed to emit step_progress event: {}", e);
                                        }
                                        // Also broadcast to WebSocket clients (channel: "step-progress")
                                        crate::event_system::broadcast_ws_notification(
                                            &app_handle_progress,
                                            "step-progress",
                                            &progress_event,
                                        );
                                    }));

                                let progress_msg = if let Some(total) = parsed_progress.total {
                                    let pct = if total > 0 {
                                        (parsed_progress.current as f64 / total as f64 * 100.0)
                                            as u32
                                    } else {
                                        0
                                    };
                                    format!(
                                        "Progress: {}/{} ({}%) - {}",
                                        parsed_progress.current,
                                        total,
                                        pct,
                                        parsed_progress.marker_type
                                    )
                                } else {
                                    format!(
                                        "Progress: {} - {}",
                                        parsed_progress.current, parsed_progress.marker_type
                                    )
                                };

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_progress,
                                            &progress_msg,
                                            "progress",
                                            None,
                                            session_ctx_for_progress.as_ref(),
                                        );
                                    }));
                            }
                            Err(e) => {
                                warn!("Failed to store progress marker: {}", e);
                            }
                        }
                    }
                }
            } else {
                while progress_rx.recv().is_ok() {}
            }

            progress_count
        });

        // Shared stderr buffer — stderr thread writes, waiter thread reads after exit
        let shared_stderr = Arc::new(Mutex::new(String::new()));
        let shared_stderr_for_reader = shared_stderr.clone();

        // Stderr reader thread
        let stderr_handle = thread::spawn(move || {
            let mut output = String::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_string(&mut output);
            }
            // Write to shared buffer for waiter thread access
            if let Ok(mut buf) = shared_stderr_for_reader.lock() {
                *buf = output.clone();
            }
            output
        });

        // Spawn a background thread to wait for child process exit
        // and manage final cleanup / state transitions.
        let state_for_waiter = state_tracker.clone();
        let writer_for_waiter = stdin_writer.clone();
        let session_id_for_waiter = session_id.to_string();
        let pid_tracker_for_waiter = pid_tracker.clone();
        let child_pid_for_waiter = child_pid;
        let app_handle_for_waiter = app_handle.clone();
        let session_ctx_for_waiter = session_ctx.clone();
        let shared_stderr_for_waiter = shared_stderr;
        // Memory federation: clone the federation context into the
        // waiter so reconcile fires when the subprocess actually exits
        // (NOT when `spawn()` returns, which is just after init).
        let federation_ctx_for_waiter = federation_ctx.clone();

        thread::spawn(move || {
            // Wait for the child process to exit
            Self::wait_for_child(child, &session_id_for_waiter);

            // Remove PID from tracker
            if let Some(ref tracker) = pid_tracker_for_waiter {
                if let Ok(mut pids) = tracker.lock() {
                    pids.retain(|&p| p != child_pid_for_waiter);
                    info!("Unregistered AI process PID {}", child_pid_for_waiter);
                }
            }

            // Close stdin
            writer_for_waiter.close();

            // Brief pause to let the stderr reader thread finish
            thread::sleep(Duration::from_millis(200));

            // ── Memory federation: session-end reconcile ─────────────
            //
            // Fires AFTER the subprocess exits (not after `spawn()`
            // returns — `spawn` returns once init handshakes, the
            // session continues until the CLI itself exits). Stops
            // the watcher (idempotent), re-snapshots the memory dir,
            // pushes any deltas the watcher missed, broadcasts a
            // Tauri event for the React frontend banner. Plan Phase 5.D.
            if let Some(ctx) = federation_ctx_for_waiter.as_ref() {
                for b in qontinui_runner_lib::observable_bridge::global_registry() {
                    let category = b.category();
                    let report = Self::block_on_async(async {
                        b.stop_watching(ctx.session_id).await;
                        b.reconcile(ctx).await
                    });
                    // Memory keeps its full Tauri-banner + coord telemetry;
                    // other categories log-and-continue.
                    if category == "memory" {
                        super::federation::emit_federation_report(
                            &app_handle_for_waiter,
                            ctx,
                            report,
                        );
                    } else {
                        super::federation::log_bridge_report(category, ctx, report);
                    }
                }
            }

            // Check if the exit was due to rate-limiting
            let stderr_output = shared_stderr_for_waiter
                .lock()
                .ok()
                .map(|s| s.clone())
                .unwrap_or_default();

            let is_rate_limited = crate::ai_provider::retry::is_rate_limit_error(&stderr_output);

            if is_rate_limited {
                warn!(
                    "Session {} exited due to rate-limit, attempting auto-restart on another account",
                    session_id_for_waiter
                );

                // Emit a user-visible notification
                if let Some(ref ctx) = session_ctx_for_waiter {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(
                            &app_handle_for_waiter,
                            "Rate limit reached. Switching account and restarting session...",
                            "status",
                            None,
                            Some(ctx),
                        );
                    }));
                }

                // Attempt auto-restart on another account
                Self::auto_restart_on_rate_limit(
                    &app_handle_for_waiter,
                    &session_id_for_waiter,
                    session_ctx_for_waiter.as_ref(),
                );
            }

            // Transition to Closed
            state_for_waiter.force_close();
            info!(
                "Session {} process exited, state -> Closed (rate_limited={})",
                session_id_for_waiter, is_rate_limited
            );
        });

        // Wait briefly for the init handshake to complete
        // The dispatcher handles the ControlResponse and transitions to Ready
        let init_deadline = Instant::now() + Duration::from_secs(60);
        let mut stderr_handle = Some(stderr_handle);
        loop {
            let current_state = state_tracker.get();
            if current_state == SessionState::Ready {
                info!("Session {} initialization complete", session_id);
                break;
            }
            if current_state == SessionState::Closed {
                // Try to capture stderr for diagnostics
                let stderr_output = stderr_handle
                    .take()
                    .and_then(|h| h.join().ok())
                    .unwrap_or_default();
                let stderr_trimmed = stderr_output.trim();
                if stderr_trimmed.is_empty() {
                    return Err("Claude CLI exited during initialization".to_string());
                } else {
                    warn!("Claude CLI stderr: {}", stderr_trimmed);
                    return Err(format!(
                        "Claude CLI exited during initialization: {}",
                        stderr_trimmed
                    ));
                }
            }
            if Instant::now() > init_deadline {
                return Err("Initialization timeout (60s) - Claude CLI did not respond".to_string());
            }
            thread::sleep(Duration::from_millis(50));
        }
        // stderr_handle is only consumed in the early-exit error branch above;
        // reaching this point means initialization succeeded and the handle is still present.
        let stderr_handle = stderr_handle
            .ok_or_else(|| "Internal error: stderr handle consumed unexpectedly".to_string())?;

        // Compute holder_name (matches dispatcher.rs:398-404's derivation
        // for file-lock event emits). For workflow sessions this is the
        // human-readable `session_name`; for terminal-launched sessions
        // (no session_ctx) it falls back to the session_id, which equals
        // the task_run_id in that path.
        let holder_name = session_ctx
            .as_ref()
            .map(|c| c.session_name.clone())
            .unwrap_or_else(|| session_id.to_string());

        Ok(Self {
            session_id: session_id.to_string(),
            holder_name,
            state_tracker,
            stdin_writer,
            pending_messages,
            accumulated_output,
            user_has_interacted,
            child_pid,
            last_activity,
            has_output,
            stop_heartbeat: Some(stop_tx),
            stdout_join: Mutex::new(Some(stdout_handle)),
            stderr_join: Mutex::new(Some(stderr_handle)),
            heartbeat_join: Mutex::new(Some(heartbeat_handle)),
            finding_join: Mutex::new(Some(finding_handle)),
            progress_join: Mutex::new(Some(progress_handle)),
            shared_output_buf,
            #[cfg(target_os = "windows")]
            stdout_raw_handle,
            pid_tracker,
            completion_tx: Mutex::new(None),
            persisted_output_len,
            turn_persist_tx: turn_persist_tx_option,
            worktree,
            federation_ctx,
        })
    }

    /// Bridge sync → async for the memory-federation calls made from
    /// the spawn site (pull / start_watcher) and the wait-for-child
    /// thread (stop_watcher / reconcile). Prefers the caller's tokio
    /// runtime when one exists (we're typically inside Tauri's), falls
    /// back to Tauri's process-wide runtime for std-thread callers.
    pub(super) fn block_on_async<F: std::future::Future>(fut: F) -> F::Output {
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => tokio::task::block_in_place(|| rt.block_on(fut)),
            Err(_) => tauri::async_runtime::block_on(fut),
        }
    }

    /// Get the current session state.
    pub fn state(&self) -> SessionState {
        self.state_tracker.get()
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the child process PID.
    pub fn pid(&self) -> u32 {
        self.child_pid
    }

    /// Get the last-activity tracker Arc (for Doctor health monitoring).
    pub fn last_activity_tracker(&self) -> Arc<AtomicU64> {
        self.last_activity.clone()
    }

    /// Friendly display name for this session.
    ///
    /// This is the same string the file-lock dispatcher emits as
    /// `holder_name` on `file-lock-*` events (see
    /// `claude_session/dispatcher.rs:398-404`): for workflow-spawned
    /// sessions it is `AiSessionContext::session_name` (e.g.
    /// `"My Workflow - Iteration 3"`); for terminal-launched sessions
    /// it falls back to `session_id`.
    pub fn holder_name(&self) -> &str {
        &self.holder_name
    }

    /// Whether a user has interacted with this session.
    pub fn has_user_interacted(&self) -> bool {
        self.user_has_interacted.load(Ordering::Relaxed)
    }

    /// Get a reference to this session's worktree metadata, if any.
    ///
    /// Returns `None` for sessions running in the original working directory.
    /// Returns `Some(&info)` after `promote_to_worktree()` has succeeded — the
    /// CLI process for this session has its `cwd` set to `info.path`.
    ///
    /// Phase 3 callers in dispatcher.rs use this to scope file-registry
    /// entries: `session.worktree().map(|w| w.id.clone())`.
    pub fn worktree(&self) -> Option<&WorktreeInfo> {
        self.worktree.as_ref()
    }

    /// Send the initial prompt to start the first turn.
    /// Transitions from Ready to Processing.
    pub fn send_initial_prompt(&self, prompt: &str) -> Result<(), String> {
        let state = self.state_tracker.get();
        if state != SessionState::Ready {
            return Err(format!("Cannot send initial prompt in state: {}", state));
        }

        let user_msg = UserInputMessage::new(prompt, "default");
        self.stdin_writer.write_message(&user_msg)?;

        self.state_tracker
            .transition(SessionState::Processing)
            .map_err(|e| format!("State transition failed: {}", e))?;

        info!(
            "Sent initial prompt ({} chars), session {} -> Processing",
            prompt.len(),
            self.session_id
        );

        Ok(())
    }

    /// Send a user message. If Ready, sends immediately. If Processing, queues it.
    /// Returns true if sent immediately, false if queued.
    pub fn send_user_message(&self, message: &str) -> Result<bool, String> {
        let state = self.state_tracker.get();

        if !state.can_send_message() {
            return Err(format!("Cannot send message in state: {}", state));
        }

        if state == SessionState::Ready {
            // Send immediately
            let user_msg = UserInputMessage::new(message, "default");
            self.stdin_writer.write_message(&user_msg)?;
            self.user_has_interacted.store(true, Ordering::Relaxed);

            self.state_tracker
                .transition(SessionState::Processing)
                .map_err(|e| format!("State transition failed: {}", e))?;

            info!(
                "Sent user message immediately ({} chars), session {} -> Processing",
                message.len(),
                self.session_id
            );
            Ok(true)
        } else {
            // Queue the message (Processing state)
            let mut queue = self
                .pending_messages
                .lock()
                .map_err(|e| format!("Failed to lock message queue: {}", e))?;

            if queue.len() >= MAX_PENDING_MESSAGES {
                return Err(format!(
                    "Message queue full ({} messages). Wait for the current turn to complete.",
                    MAX_PENDING_MESSAGES
                ));
            }

            queue.push_back(message.to_string());
            info!(
                "Queued user message ({} chars, {} in queue)",
                message.len(),
                queue.len()
            );
            Ok(false)
        }
    }

    /// Send an interrupt request.
    pub fn interrupt(&self) -> Result<(), String> {
        let state = self.state_tracker.get();
        if !state.can_interrupt() {
            return Err(format!("Cannot interrupt in state: {}", state));
        }

        let request_id = next_request_id();
        let interrupt_msg = OutgoingControlRequest::interrupt(&request_id);
        self.stdin_writer.write_message(&interrupt_msg)?;

        self.state_tracker
            .transition(SessionState::Interrupting)
            .map_err(|e| format!("State transition failed: {}", e))?;

        info!(
            "Sent interrupt request (id: {}), session {} -> Interrupting",
            request_id, self.session_id
        );
        Ok(())
    }

    /// Get the accumulated output text.
    pub fn get_output(&self) -> String {
        self.accumulated_output
            .lock()
            .map(|buf| buf.clone())
            .unwrap_or_default()
    }

    /// Wait for the session to reach a terminal state.
    /// Returns (success, accumulated_output).
    pub fn wait_for_completion(&self, timeout: Option<Duration>) -> Result<(bool, String), String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut was_processing = self.state_tracker.get() == SessionState::Processing;

        loop {
            let state = self.state_tracker.get();
            if state == SessionState::Closed {
                // Use shared_output_buf rather than accumulated_output, which is
                // drained after each turn for memory efficiency. The shared buffer
                // contains the last synced snapshot from the stdout reader thread.
                let output = self
                    .shared_output_buf
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                // Consider it successful if we got output
                let success = !output.is_empty();
                return Ok((success, output));
            }

            // In interactive mode, Claude CLI stays alive after sending a result
            // (state goes Processing -> Ready). If we were processing and now we're
            // Ready with no pending messages and no user interaction, close stdin to
            // signal Claude to exit. The child waiter thread will then set Closed.
            if was_processing && state == SessionState::Ready {
                let has_pending = self
                    .pending_messages
                    .lock()
                    .map(|q| !q.is_empty())
                    .unwrap_or(false);
                let user_interacted = self.user_has_interacted.load(Ordering::Relaxed);

                if !has_pending && !user_interacted {
                    info!(
                        "Session {} turn completed (Processing -> Ready), closing stdin to exit CLI",
                        self.session_id
                    );
                    self.stdin_writer.close();
                    // Reset flag and continue waiting for Closed state
                    was_processing = false;
                }
            }

            if state == SessionState::Processing {
                was_processing = true;
            }

            if let Some(deadline) = deadline {
                if Instant::now() > deadline {
                    warn!(
                        "Session {} wait timeout, current state: {}",
                        self.session_id, state
                    );
                    // Return what we have
                    let output = self
                        .shared_output_buf
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();
                    return Ok((false, output));
                }
            }

            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Flush any unpersisted accumulated output to `output_log` WITHOUT
    /// closing the session.
    ///
    /// Used by the graceful-drain sequence (`crate::drain`) to ensure an
    /// in-flight turn's text reaches the DB before a planned restart, so the
    /// resume replay sees it. Idempotent vs. the dispatcher's own per-turn
    /// flush (both guard on `persisted_output_len`): a turn that already
    /// checkpointed Processing → Ready flushes nothing extra here.
    ///
    /// Mirrors the flush block in [`Self::close`], but leaves stdin, the state
    /// machine, and the persister thread alive — the session keeps running
    /// until the seam's `close_all_sessions` tears it down.
    pub fn flush_pending_output(&self) {
        if let Some(ref tx) = self.turn_persist_tx {
            if let Ok(buf) = self.accumulated_output.lock() {
                // Reserve the persisted region atomically so a concurrent
                // dispatcher flush for the same content can't double-send.
                let persisted = self.persisted_output_len.swap(usize::MAX, Ordering::SeqCst);
                if buf.len() > persisted && persisted != usize::MAX {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        let _ = tx.send(delta);
                    }
                }
                // Do NOT clear the buffer — the session stays alive and may
                // produce more output before the seam closes it (close() will
                // flush the remainder and clear).
            }
        }
    }

    /// Gracefully close the session.
    pub fn close(&self) -> Result<(), String> {
        info!("Closing session {}", self.session_id);

        // Stop heartbeat
        if let Some(ref tx) = self.stop_heartbeat {
            let _ = tx.send(());
        }

        // Flush any remaining unpersisted output before closing.
        // Set persisted_output_len to MAX first to prevent the dispatcher from
        // also sending a delta for the same content (race with stdout thread).
        if let Some(ref tx) = self.turn_persist_tx {
            if let Ok(mut buf) = self.accumulated_output.lock() {
                let persisted = self.persisted_output_len.swap(usize::MAX, Ordering::SeqCst);
                if buf.len() > persisted {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        let _ = tx.send(delta);
                    }
                }
                buf.clear(); // Free memory on close
            }
            // Note: the sender is dropped when the ClaudeSession struct is dropped,
            // which terminates the persister thread after it processes pending messages.
        }

        // Close stdin (sends EOF to CLI)
        self.stdin_writer.close();

        // Force state to Closed
        self.state_tracker.force_close();

        // Close stdout pipe on Windows to unblock reader thread
        #[cfg(target_os = "windows")]
        if let Some(raw_handle) = self.stdout_raw_handle {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    raw_handle as windows_sys::Win32::Foundation::HANDLE,
                );
            }
        }

        info!("Session {} closed", self.session_id);
        Ok(())
    }

    /// Read this session's task output log from PG and build a replay prompt.
    ///
    /// Shared by `auto_restart_on_rate_limit` (account rotation) and
    /// `promote_to_worktree` (worktree promotion) — both kill-and-respawn the
    /// CLI process and need to give the new instance the prior conversation.
    ///
    /// Returns:
    /// - `Ok(Some(prompt))` — there is history; send this as the initial prompt.
    /// - `Ok(None)` — no `session_ctx`, empty output_log, or no parseable turns.
    /// - `Err(BuildReplayErr::NoPg)` — PG global isn't initialized; caller
    ///   typically aborts the respawn since persistence won't work either way.
    fn build_replay_from_history(
        session_id: &str,
        session_ctx: Option<&AiSessionContext>,
    ) -> Result<Option<String>, BuildReplayErr> {
        use crate::claude_session::resume::{build_replay_prompt, parse_conversation};

        let ctx = match session_ctx {
            Some(c) => c,
            None => return Ok(None),
        };

        let task_run_id = ctx.task_run_id();
        let pg = match crate::database::pg::PgDb::try_global() {
            Some(pg) => pg,
            None => return Err(BuildReplayErr::NoPg),
        };

        let output_log =
            tauri::async_runtime::block_on(pg.get_task_output(task_run_id)).unwrap_or_default();

        if output_log.is_empty() {
            info!(
                "No conversation history to replay for session {}",
                session_id
            );
            return Ok(None);
        }

        let turns = parse_conversation(&output_log);
        if turns.is_empty() {
            return Ok(None);
        }

        let replay = build_replay_prompt(&turns, None);
        info!(
            "Built replay prompt for session {} ({} turns, {} chars)",
            session_id,
            turns.len(),
            replay.len()
        );
        Ok(Some(replay))
    }

    /// Promote a running session into an isolated git worktree.
    ///
    /// Sequence:
    /// 1. Guard against double-promotion (early return if `worktree.is_some()`).
    /// 2. Transition state Ready -> Promoting.
    /// 3. Create the worktree on disk via `crate::worktree::create_worktree`.
    /// 4. Persist a `worktrees` row (id = session_id + "-wt") via PG.
    /// 5. Read conversation history and build a replay prompt (shared helper).
    /// 6. Close the existing CLI process (cwd is immutable — kill + respawn).
    /// 7. Spawn a fresh CLI with `cwd = worktree.path` and `worktree = Some(...)`.
    /// 8. Send the replay prompt so the user does not lose context.
    /// 9. Swap the new session into `*self` (state ends in Ready via spawn handshake).
    ///
    /// Phase 4 will expose this via an MCP command. Phase 3's dispatcher reads
    /// `self.worktree()` so file-registry entries are scoped to the worktree.
    pub async fn promote_to_worktree(
        &mut self,
        repo_path: &Path,
        app_handle: &tauri::AppHandle,
        session_ctx: Option<AiSessionContext>,
    ) -> Result<WorktreeInfo, String> {
        // 1. Already promoted? Return existing metadata unchanged.
        if let Some(existing) = &self.worktree {
            info!(
                "Session {} already promoted to worktree {} (branch={}); skipping",
                self.session_id, existing.id, existing.branch
            );
            return Ok(existing.clone());
        }

        // 2. Transition Ready -> Promoting (other states are user errors).
        self.state_tracker
            .transition(SessionState::Promoting)
            .map_err(|e| format!("promote_to_worktree: {}", e))?;

        // 3. Create the worktree on disk. Branch name is derived from session_id.
        let create_result =
            crate::worktree::create_worktree(repo_path, &self.session_id, &self.session_id)
                .map_err(|e| {
                    // Try to roll the state back so the session can keep working.
                    let _ = self.state_tracker.transition(SessionState::Ready);
                    format!("worktree creation failed: {}", e)
                })?;

        let worktree_id = format!("{}-wt", self.session_id);
        let worktree_path = create_result.worktree_path.clone();
        let worktree_path_str = worktree_path.to_string_lossy().to_string();
        let branch_name = create_result.branch_name.clone();

        // 4. Persist the worktree row. Best-effort: if PG fails we still proceed,
        //    but log loudly — without a row Phase 3's file-registry scoping is
        //    a no-op for this session.
        let task_run_id = session_ctx.as_ref().map(|c| c.task_run_id().to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let record = crate::worktree::WorktreeRecord {
            id: worktree_id.clone(),
            worktree_path: worktree_path_str.clone(),
            branch_name: branch_name.clone(),
            source_branch: create_result.source_branch.clone(),
            source_commit: create_result.source_commit.clone(),
            repo_path: repo_path.to_string_lossy().to_string(),
            task_run_id,
            workflow_name: Some(self.session_id.clone()),
            status: crate::worktree::WorktreeStatus::Active,
            created_at: now.clone(),
            updated_at: now,
        };
        if let Some(pg) = crate::database::pg::PgDb::try_global() {
            if let Err(e) = pg.insert_worktree(&record).await {
                warn!(
                    "promote_to_worktree: failed to persist worktrees row {}: {}",
                    worktree_id, e
                );
            }
        } else {
            warn!(
                "promote_to_worktree: no PG global; worktrees row {} not persisted",
                worktree_id
            );
        }

        let info = WorktreeInfo {
            id: worktree_id,
            path: worktree_path,
            branch: branch_name,
        };

        // 5. Read prior conversation so the new CLI can replay it.
        let replay_prompt =
            match Self::build_replay_from_history(&self.session_id, session_ctx.as_ref()) {
                Ok(prompt) => prompt,
                Err(BuildReplayErr::NoPg) => {
                    warn!(
                        "promote_to_worktree: no PG; respawning without replay context for {}",
                        self.session_id
                    );
                    None
                }
            };

        // 6. Kill the existing CLI. cwd is immutable; this is the only way to
        //    move the process into the worktree.
        if let Err(e) = self.close() {
            warn!(
                "promote_to_worktree: close() returned {} (continuing anyway)",
                e
            );
        }

        // 7. Spawn a fresh CLI with cwd = worktree.path and worktree metadata.
        let session_id = self.session_id.clone();
        let new_session = Self::spawn(
            &worktree_path_str,
            &session_id,
            app_handle,
            session_ctx,
            None, // finding_ctx — re-attached by caller if needed
            None, // progress_ctx
            self.pid_tracker.clone(),
            None, // model_override
            Some(info.clone()),
            None, // tool_policy — worktree promotion preserves the original spawn's policy state
        )
        .map_err(|e| format!("promote_to_worktree: respawn failed: {}", e))?;

        // 8. Replay history, if any, BEFORE swapping into self. This way if the
        //    initial prompt fails we know the in-place swap hasn't happened yet
        //    and the caller gets a clean error.
        if let Some(ref prompt) = replay_prompt {
            if let Err(e) = new_session.send_initial_prompt(prompt) {
                warn!(
                    "promote_to_worktree: failed to send replay prompt: {} (proceeding)",
                    e
                );
            }
        }

        // 9. Swap into self. The old struct's Drop runs, but state is already
        //    Closed (force_close in close()), so it's a no-op.
        *self = new_session;

        info!(
            "Session {} promoted to worktree {} (branch={}, path={})",
            self.session_id, info.id, info.branch, worktree_path_str
        );

        Ok(info)
    }

    /// Auto-restart the session on another account after a rate-limit exit.
    ///
    /// Reads conversation history from the DB output_log, rotates the account,
    /// spawns a new ClaudeSession with a replay prompt, and re-registers it
    /// in the SessionManager.
    fn auto_restart_on_rate_limit(
        app_handle: &tauri::AppHandle,
        session_id: &str,
        session_ctx: Option<&AiSessionContext>,
    ) {
        use crate::ai_provider::{get_effective_config_dir, rotate_account_on_rate_limit};
        use tauri::Manager;

        // 1. Rotate to another account
        if !rotate_account_on_rate_limit() {
            warn!(
                "Session {}: no alternative account available for restart",
                session_id
            );
            return;
        }

        let new_config_dir =
            get_effective_config_dir(&crate::settings::get_ai_settings().claude_cli)
                .unwrap_or_else(|| "unknown".to_string());
        let label = std::path::Path::new(&new_config_dir)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&new_config_dir);
        info!(
            "Session {}: restarting on account '{}' after rate-limit",
            session_id, label
        );

        // 2. Read conversation history from DB
        let conversation_context = match Self::build_replay_from_history(session_id, session_ctx) {
            Ok(replay) => replay,
            Err(BuildReplayErr::NoPg) => {
                warn!("No PG connection for reading conversation history");
                return;
            }
        };

        // 3. Spawn new session
        let sm = match app_handle.try_state::<Arc<crate::claude_session::manager::SessionManager>>()
        {
            Some(sm) => sm.inner().clone(),
            None => {
                warn!("SessionManager not available for auto-restart");
                return;
            }
        };

        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Clone session_ctx for the new session
        let new_session_ctx = session_ctx.cloned();

        match Self::spawn(
            &working_dir,
            session_id,
            app_handle,
            new_session_ctx,
            None, // finding_ctx
            None, // progress_ctx
            None, // pid_tracker
            None, // model_override
            None, // worktree (rate-limit restart preserves the original cwd)
            None, // tool_policy (rate-limit restart preserves the original policy-less spawn)
        ) {
            Ok(new_session) => {
                let new_session = Arc::new(new_session);

                // Send replay prompt if we have conversation context
                if let Some(ref replay_prompt) = conversation_context {
                    if let Err(e) = new_session.send_initial_prompt(replay_prompt) {
                        warn!("Failed to send replay prompt: {}", e);
                    }
                }

                // Remove old session and re-register under the same ID
                sm.remove(session_id);
                if let Err(e) = sm.register(session_id, new_session.clone()) {
                    warn!("Failed to re-register session after restart: {}", e);
                    return;
                }

                // Emit state event so frontend knows the session is back
                crate::commands::ai_session::emit_session_state_ex(
                    app_handle,
                    session_id,
                    session_id,
                    new_session.state(),
                    true, // resumed flag
                );

                // Notify user
                if let Some(ctx) = session_ctx {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(
                            app_handle,
                            &format!(
                                "Session restarted on account '{}'. Conversation context restored.",
                                label
                            ),
                            "status",
                            None,
                            Some(ctx),
                        );
                    }));
                }

                info!(
                    "Session {} successfully restarted on account '{}'",
                    session_id, label
                );
            }
            Err(e) => {
                warn!(
                    "Failed to restart session {} on new account: {}",
                    session_id, e
                );
                // Notify user of failure
                if let Some(ctx) = session_ctx {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(
                            app_handle,
                            &format!("Failed to restart session on new account: {}", e),
                            "error",
                            None,
                            Some(ctx),
                        );
                    }));
                }
            }
        }
    }

    /// Internal: wait for child process to exit.
    fn wait_for_child(mut child: Child, session_id: &str) {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    info!("Session {} child process exited: {:?}", session_id, status);
                    return;
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    error!("Session {} error waiting for child: {}", session_id, e);
                    return;
                }
            }
        }
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        // Ensure cleanup on drop
        if self.state_tracker.get() != SessionState::Closed {
            debug!(
                "ClaudeSession::drop - forcing close for {}",
                self.session_id
            );
            let _ = self.close();
        }

        // Remove PID from tracker
        if let Some(ref tracker) = self.pid_tracker {
            if let Ok(mut pids) = tracker.lock() {
                pids.retain(|&p| p != self.child_pid);
            }
        }
    }
}

impl std::fmt::Debug for ClaudeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeSession")
            .field("session_id", &self.session_id)
            .field("state", &self.state_tracker.get())
            .field("pid", &self.child_pid)
            .field("user_interacted", &self.has_user_interacted())
            .finish()
    }
}
