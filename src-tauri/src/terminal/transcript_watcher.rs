//! Transcript-tail populator for PTY-launched AI tabs (Phase 1.5).
//!
//! Watches Claude CLI's per-session JSONL transcripts on disk
//! (`<config_dir>/projects/<encoded-project>/<session_id>.jsonl`), parses
//! `Edit` / `Write` / `MultiEdit` `tool_use` blocks out of them, and calls
//! `pg.record_file_touched(session_id, file_path, None)` so that the
//! `coord.session_touched_files` table populates for PTY tabs the same way
//! it already does for SDK chat sessions via `auto_register_file`.
//!
//! Why this exists: the dominant terminal-AI launch path
//! (`onLaunchAiSession` at `TerminalPage.tsx:389-424`) types `claude` into a
//! PTY shell. The Claude CLI runs as a child of the PTY, NOT of
//! `ClaudeSession::spawn`, so its tool-use stream is never read by the runner
//! and `auto_register_file` never fires for those tabs. Without this watcher,
//! the §1 traffic light has no rows to read for the dominant UX path.
//!
//! Reuses existing infrastructure:
//!   - `terminal::transcript::find_claude_config_dirs()` for config dirs.
//!   - `terminal::transcript::list_sessions()` for startup discovery.
//!   - `terminal::transcript::is_workflow_session_marker()` for the
//!     workflow-vs-interactive filter.
//!   - `terminal::transcript::parse_line_for_touched_files()` for parsing.
//!
//! Mirrors the `notify::RecommendedWatcher` + tokio-channel pattern from
//! `trigger_system/watchers/file_watcher.rs` and `wrappers/registry.rs`.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::OnceCell;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::{mpsc, Mutex as TokioMutex, Notify};
use tracing::{debug, info, warn};

use super::transcript::{
    find_claude_config_dirs, is_workflow_session_marker, list_sessions, parse_line_for_agent_log,
    parse_line_for_touched_files, AgentLogObs,
};
use crate::claude_session::coord_register::{AgentLogEmitter, AiCoordRegistrar};

/// Ceiling for "recent enough to schedule a tail task on startup". Older
/// JSONLs are skipped — they belong to closed sessions whose tabs the user
/// can no longer act on.
const STARTUP_RECENCY: Duration = Duration::from_secs(24 * 60 * 60);

/// Per-tail-task fallback poll interval. Defends a missed `Modify` event from
/// `notify` by ensuring the loop runs at least this often even without a
/// wake.
const TAIL_FALLBACK_TICK: Duration = Duration::from_secs(1);

/// Hold the watcher for the app's lifetime. `notify::RecommendedWatcher` is
/// `Drop`-stop-the-watcher, so we keep it alive in a `OnceCell`. The watcher
/// is opaque to the rest of the program; its events flow through an
/// `mpsc::Sender` instead. The std::sync::Mutex is never actually locked
/// after `set()` — it's just a `Send + Sync` wrapper for the
/// not-necessarily-Sync watcher handle.
static WATCHER: OnceCell<std::sync::Mutex<Vec<RecommendedWatcher>>> = OnceCell::new();

/// Errors a tail task can return. Non-fatal — the supervisor logs and
/// proceeds.
#[derive(Debug, thiserror::Error)]
enum TailError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Handle to a running tail task. Wakes the task on `notify`-fired events and
/// supports clean cancellation.
struct TailHandle {
    wake: Arc<Notify>,
    cancel: Arc<Notify>,
}

/// Public entry point. Spawns the watcher + tail tasks asynchronously and
/// returns immediately. The watcher lives until process exit; do not call
/// twice for the same process.
///
/// `workspace_paths` is the list of project paths the runner currently tracks
/// (typically a single workspace root). Each path is matched against every
/// discovered config dir's `projects/<encoded>/` folder.
pub fn start_transcript_watcher(
    app_handle: tauri::AppHandle,
    pg: Arc<crate::database::pg::PgDb>,
    workspace_paths: Vec<String>,
    registrar: Option<Arc<AiCoordRegistrar>>,
) -> Result<(), String> {
    if WATCHER.get().is_some() {
        return Err("transcript watcher already started".to_string());
    }

    info!(
        "transcript_watcher: starting with {} workspace path(s)",
        workspace_paths.len()
    );

    // Spawn the orchestrator on the tokio runtime. It owns the channel, the
    // watcher, and the per-session task map.
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_orchestrator(app_handle, pg, workspace_paths, registrar).await {
            warn!("transcript_watcher: orchestrator exited with error: {}", e);
        }
    });

    Ok(())
}

/// Long-lived task: builds the `notify` watcher per config dir, schedules
/// startup tail tasks, and dispatches filesystem events to per-session
/// handles.
async fn run_orchestrator(
    app_handle: tauri::AppHandle,
    pg: Arc<crate::database::pg::PgDb>,
    workspace_paths: Vec<String>,
    registrar: Option<Arc<AiCoordRegistrar>>,
) -> Result<(), String> {
    let config_dirs = find_claude_config_dirs();
    if config_dirs.is_empty() {
        info!("transcript_watcher: no Claude config dirs found; nothing to watch");
        return Ok(());
    }

    // session_id -> handle
    let tasks: Arc<TokioMutex<HashMap<String, TailHandle>>> =
        Arc::new(TokioMutex::new(HashMap::new()));

    // ── 1. Discovery on startup ───────────────────────────────────────────
    //
    // For every (config_dir × workspace_path) pair, list current sessions and
    // schedule a tail starting from EOF for any whose mtime is within the
    // last STARTUP_RECENCY.
    let now = SystemTime::now();
    for config_dir in &config_dirs {
        for project in &workspace_paths {
            let sessions = match list_sessions(config_dir, project) {
                Ok(s) => s,
                Err(e) => {
                    debug!(
                        "transcript_watcher: list_sessions({:?}, {}) failed: {}",
                        config_dir, project, e
                    );
                    continue;
                }
            };
            for s in sessions {
                let jsonl = config_dir
                    .join("projects")
                    .join(encode_for_lookup(project))
                    .join(format!("{}.jsonl", s.session_id));
                if !jsonl.exists() {
                    continue;
                }
                let mtime_ok = std::fs::metadata(&jsonl)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(|t| now.duration_since(t).unwrap_or_default() <= STARTUP_RECENCY)
                    .unwrap_or(false);
                if !mtime_ok {
                    continue;
                }
                schedule_tail(
                    &tasks,
                    s.session_id.clone(),
                    jsonl,
                    pg.clone(),
                    app_handle.clone(),
                    registrar.clone(),
                    /* start_at_eof */ true,
                )
                .await;
            }
        }
    }

    // ── 2. Live watching ──────────────────────────────────────────────────
    //
    // One `mpsc` channel for all config dirs. Each `notify` callback runs in
    // a sync context so we use `try_send`.
    let (tx, mut rx) = mpsc::channel::<Event>(128);

    let mut watchers: Vec<RecommendedWatcher> = Vec::new();
    for config_dir in &config_dirs {
        let projects_root = config_dir.join("projects");
        if !projects_root.exists() {
            continue;
        }
        let tx_for_handler = tx.clone();
        let watcher_result =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                if let Ok(ev) = res {
                    let _ = tx_for_handler.try_send(ev);
                }
            });
        let mut watcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                warn!(
                    "transcript_watcher: failed to create watcher for {:?}: {}",
                    projects_root, e
                );
                continue;
            }
        };
        if let Err(e) = watcher.watch(&projects_root, RecursiveMode::Recursive) {
            warn!(
                "transcript_watcher: failed to watch {:?}: {}",
                projects_root, e
            );
            continue;
        }
        info!("transcript_watcher: watching {:?}", projects_root);
        watchers.push(watcher);
    }

    // Stash watchers so they live until process exit. Re-entry into
    // `start_transcript_watcher` is rejected by the `OnceCell` guard above.
    let _ = WATCHER.set(std::sync::Mutex::new(watchers));

    // ── 3. Event dispatch loop ────────────────────────────────────────────
    while let Some(event) = rx.recv().await {
        for path in &event.paths {
            // Only attend to *.jsonl files inside a `projects/` subtree.
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            match event.kind {
                EventKind::Create(_) => {
                    // Create: read first 5 lines, run workflow filter; if
                    // interactive, schedule a tail starting at offset 0.
                    if path_looks_like_workflow_session(path) {
                        debug!(
                            "transcript_watcher: skipping workflow session {} ({})",
                            session_id,
                            path.display()
                        );
                        continue;
                    }
                    schedule_tail(
                        &tasks,
                        session_id.clone(),
                        path.clone(),
                        pg.clone(),
                        app_handle.clone(),
                        registrar.clone(),
                        /* start_at_eof */ false,
                    )
                    .await;
                }
                EventKind::Modify(_) => {
                    // Wake an existing task; if none, treat as a create
                    // (the runner may have started after the file).
                    let map = tasks.lock().await;
                    if let Some(h) = map.get(&session_id) {
                        h.wake.notify_one();
                    } else {
                        drop(map);
                        if path_looks_like_workflow_session(path) {
                            continue;
                        }
                        schedule_tail(
                            &tasks,
                            session_id.clone(),
                            path.clone(),
                            pg.clone(),
                            app_handle.clone(),
                            registrar.clone(),
                            /* start_at_eof */ true,
                        )
                        .await;
                    }
                }
                EventKind::Remove(_) => {
                    let mut map = tasks.lock().await;
                    if let Some(h) = map.remove(&session_id) {
                        h.cancel.notify_one();
                        debug!(
                            "transcript_watcher: cancelled tail for removed session {}",
                            session_id
                        );
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Cheap sniff for `is_workflow_session` without re-loading via
/// `list_sessions`. Reads up to the first 5 lines of the file. Returns true
/// if it's a workflow session OR if the file can't be read yet (which is
/// common immediately after `Create` because the writer hasn't flushed) —
/// callers fall back to spawning the task and the in-task re-check (step 7
/// in the loop) catches it after the first wake.
fn path_looks_like_workflow_session(path: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        // File not yet readable → treat as not-workflow; the in-task re-check
        // will tear down if the marker shows up later.
        Err(_) => return false,
    };
    let reader = BufReader::new(f);
    let mut buf = String::with_capacity(2048);
    for (i, line) in reader.lines().enumerate() {
        if i >= 5 {
            break;
        }
        if let Ok(l) = line {
            buf.push_str(&l);
            buf.push('\n');
        }
    }
    is_workflow_session_marker(&buf)
}

/// Spawn or replace the per-session tail task.
async fn schedule_tail(
    tasks: &Arc<TokioMutex<HashMap<String, TailHandle>>>,
    session_id: String,
    path: PathBuf,
    pg: Arc<crate::database::pg::PgDb>,
    app_handle: tauri::AppHandle,
    registrar: Option<Arc<AiCoordRegistrar>>,
    start_at_eof: bool,
) {
    let mut map = tasks.lock().await;
    if map.contains_key(&session_id) {
        // Already tailed.
        return;
    }
    let wake = Arc::new(Notify::new());
    let cancel = Arc::new(Notify::new());
    map.insert(
        session_id.clone(),
        TailHandle {
            wake: wake.clone(),
            cancel: cancel.clone(),
        },
    );
    drop(map);

    let tasks_for_cleanup = tasks.clone();
    let session_id_for_cleanup = session_id.clone();

    tauri::async_runtime::spawn(async move {
        let result = tail_session(
            session_id.clone(),
            path,
            pg,
            app_handle,
            registrar,
            wake,
            cancel,
            start_at_eof,
        )
        .await;
        if let Err(e) = result {
            warn!(
                "transcript_watcher: tail task for {} exited with error: {}",
                session_id, e
            );
        }
        // Self-remove from the registry on exit so reschedule works.
        let mut map = tasks_for_cleanup.lock().await;
        map.remove(&session_id_for_cleanup);
    });
}

/// Per-session tail loop. Reads new bytes on each wake, parses lines into
/// `TouchedFile`s, persists via `pg.record_file_touched`, and triggers a
/// debounced commit-state emit on landed rows.
async fn tail_session(
    session_id: String,
    path: PathBuf,
    pg: Arc<crate::database::pg::PgDb>,
    app_handle: tauri::AppHandle,
    registrar: Option<Arc<AiCoordRegistrar>>,
    wake: Arc<Notify>,
    cancel: Arc<Notify>,
    start_at_eof: bool,
) -> Result<(), TailError> {
    // ── 1. Open file, establish initial cursor ────────────────────────────
    let mut file = tokio::fs::File::open(&path).await?;
    let mut cursor: u64 = if start_at_eof {
        file.metadata().await.map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    // Buffered prefix used for the workflow-session re-check on first wake.
    let mut workflow_check_buf = String::new();
    let mut did_workflow_recheck = false;

    // PR-shepherd Phase 1 (plan 2026-07-04-runner-pr-shepherd): per-session
    // `gh pr create` tool_use ↔ tool_result correlator. Cheap to hold even
    // when the shepherd gate is off — the per-line feed below is gated.
    let mut pr_create_tracker = super::pr_open_report::PrCreateTracker::new();

    // ── Interactive-agent log emitter (Phase 2) ───────────────────────────────
    //
    // Stream this PTY CLI session's assistant text + tool_use activity to
    // coord's `agent_logs` ingest so it appears on `/admin/coord/agents`. The
    // emitter is keyed on the transcript's own session UUID (the `.jsonl`
    // stem); coord's Phase-1a fallback resolves the tenant from the `device_id`
    // the emitter stamps, so there is NO separate `coord.sessions` registration
    // step here. `start()` is a strict no-op (returns `None`, spawns no thread)
    // when the gate `QONTINUI_AGENT_LOGS_FROM_SESSIONS` is OFF, the `device_id`
    // is unreadable, or `session_id` isn't a UUID — so the per-line emit below
    // costs nothing in the common (gate-off) case. Held for the tail's
    // lifetime; the drain thread flushes on channel disconnect at function exit
    // (RAII).
    //
    // We emit NEITHER a `session_started` NOR a `session_closed` milestone here:
    // a single real CLI session produces MANY `tail_session` invocations (file
    // rotation/truncation re-opens the cursor, the discovery loop reschedules a
    // torn-down tail on the next `Modify`, a runner restart re-discovers every
    // live transcript), so neither a tail start nor a tail teardown is a
    // reliable session-lifecycle signal — emitting `started()` per tail floods
    // `coord.agent_logs` with duplicate `session_started` rows (~one per tail
    // re-spawn). The per-line `assistant` / `tool_use` emits below ARE the
    // activity signal, and they are inherently de-duplicated by the byte cursor
    // (only lines past `cursor` emit), so re-invocation never double-counts
    // content. The once-per-session `session_started` milestone belongs to the
    // UI-interactive spawn path (`session.rs`), which fires exactly once per
    // genuine `ClaudeSession::spawn`.
    let agent_log_emitter = uuid::Uuid::parse_str(&session_id)
        .ok()
        .and_then(AgentLogEmitter::start);

    debug!(
        "transcript_watcher: tail started for {} at offset {} ({})",
        session_id,
        cursor,
        path.display()
    );

    loop {
        // ── 2. Wait for wake or fallback tick or cancel ───────────────────
        tokio::select! {
            _ = cancel.notified() => {
                debug!("transcript_watcher: tail cancelled for {}", session_id);
                return Ok(());
            }
            _ = wake.notified() => {}
            _ = tokio::time::sleep(TAIL_FALLBACK_TICK) => {}
        }

        // ── 3. Detect rotation/truncation ─────────────────────────────────
        let len = match tokio::fs::metadata(&path).await {
            Ok(m) => m.len(),
            Err(_) => continue, // Transient — retry on next tick.
        };
        if len < cursor {
            debug!(
                "transcript_watcher: detected truncation for {} (was {} bytes, now {})",
                session_id, cursor, len
            );
            cursor = 0;
            // Re-open to reset any internal seek state.
            file = tokio::fs::File::open(&path).await?;
            workflow_check_buf.clear();
            did_workflow_recheck = false;
        }
        if len == cursor {
            continue;
        }

        // ── 4. Read appended bytes line-by-line ───────────────────────────
        if let Err(e) = file.seek(std::io::SeekFrom::Start(cursor)).await {
            warn!("transcript_watcher: seek failed for {}: {}", session_id, e);
            // Re-open and retry next tick.
            file = tokio::fs::File::open(&path).await?;
            continue;
        }
        let mut reader = BufReader::new(&mut file);
        let mut rows_landed = 0usize;
        let mut bytes_read: u64 = 0;
        let mut line = String::new();
        loop {
            line.clear();
            let n = match reader.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "transcript_watcher: read_line failed for {}: {}",
                        session_id, e
                    );
                    break;
                }
            };
            if n == 0 {
                break; // EOF
            }
            // Only fully-terminated lines count — partial trailing reads
            // are NOT added to `bytes_read`, so we re-read them on the next
            // pass once the writer flushes the trailing newline.
            if !line.ends_with('\n') {
                break;
            }
            bytes_read += n as u64;

            // Buffer the first 5 lines for the workflow re-check.
            if !did_workflow_recheck && workflow_check_buf.lines().count() < 5 {
                workflow_check_buf.push_str(&line);
            }

            match parse_line_for_touched_files(&session_id, &line) {
                Ok(touched) => {
                    for tf in touched {
                        match pg
                            .record_file_touched(&session_id, &tf.file_path, None)
                            .await
                        {
                            Ok(()) => rows_landed += 1,
                            Err(e) => warn!(
                                "transcript_watcher: record_file_touched({}, {}) failed: {}",
                                session_id, tf.file_path, e
                            ),
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "transcript_watcher: malformed JSON line in {}: {}",
                        session_id, e
                    );
                }
            }

            // ── Commit ↔ session lineage push-report (Population path 2) ──
            //
            // Detect `git push` Bash tool_use blocks on this line and, for
            // each, resolve repo/branch/SHAs and enqueue a coord outbox
            // report. Best-effort; the git enumeration runs on a blocking
            // pool so it never stalls the tail loop. Skipped entirely when no
            // registrar (outbox) is wired.
            if let Some(reg) = registrar.as_ref() {
                let pushes = super::commit_report::parse_line_for_pushes(&line);
                for obs in pushes {
                    let reg = reg.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        super::commit_report::handle_push_observation(&obs, &reg);
                    });
                }
            }

            // ── PR-shepherd Phase 1: seed PR watches on `gh pr create` ────
            //
            // Detect `gh pr create` tool_use blocks + their tool_result URLs
            // on this line and enqueue pending watch seeds the PR watcher
            // drains on its next poll (plan 2026-07-04-runner-pr-shepherd).
            // Gated on QONTINUI_PR_SHEPHERD(+_AUTOSEED), so the common
            // (off) case costs one env read per line. The branch-lookup
            // fallback runs git, so — like the push path above — the
            // handler goes to the blocking pool and never stalls the tail.
            if crate::trigger_system::pr_shepherd::autoseed_enabled() {
                for event in pr_create_tracker.observe_line(&line) {
                    let coord_session_id = registrar
                        .as_ref()
                        .and_then(|r| r.session_id_for(&session_id));
                    let identity =
                        super::pr_open_report::seed_identity(&session_id, coord_session_id);
                    tauri::async_runtime::spawn_blocking(move || {
                        super::pr_open_report::handle_pr_open_event(event, identity);
                    });
                }
            }

            // ── Interactive-agent log emit (Phase 2) ──────────────────────
            //
            // Stream assistant text + tool_use observations from this
            // transcript line to coord. Pushing is a non-blocking channel
            // send (safe from async); the gate/device-id check already
            // happened in `start()`, so this is a strict no-op when off.
            if let Some(em) = agent_log_emitter.as_ref() {
                for obs in parse_line_for_agent_log(&line) {
                    match obs {
                        AgentLogObs::Assistant { text } => {
                            em.emit("info", "assistant", Some(json!({ "text": text })));
                        }
                        AgentLogObs::ToolUse { tool, input } => {
                            let mut payload = json!({ "tool": tool });
                            if let Some(input) = input {
                                payload["input"] = json!(input);
                            }
                            em.emit("info", "tool_use", Some(payload));
                        }
                    }
                }
            }
        }
        cursor += bytes_read;

        // ── 5. Trigger commit-state emit if rows landed ───────────────────
        if rows_landed > 0 {
            crate::mcp::ai_session::emit_commit_state_for_session(
                app_handle.clone(),
                session_id.clone(),
            );
        }

        // ── 6. Workflow-session re-check on first complete wake ───────────
        //
        // Creation-time discovery may race the writer: the file existed at
        // `Create` but the `queue-operation` marker hadn't landed yet.
        // After the first read pass we have the buffered prefix; if the
        // marker is there, this is a workflow session — log and tear down.
        if !did_workflow_recheck && !workflow_check_buf.is_empty() {
            did_workflow_recheck = true;
            if is_workflow_session_marker(&workflow_check_buf) {
                info!(
                    "transcript_watcher: tearing down tail for workflow session {} (detected post-creation)",
                    session_id
                );
                return Ok(());
            }
        }
    }
}

/// Re-encode a project path for direct lookup. Mirrors
/// `terminal::transcript::encode_project_path` (which is private). Kept tiny
/// + intentional duplication so the watcher doesn't have to re-export
/// internal helpers.
fn encode_for_lookup(project_path: &str) -> String {
    let normalized = project_path
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    normalized
        .replace(":/", "--")
        .replace([':', '/', '\\', '_'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_for_lookup_matches_transcript_encoder() {
        // Spot-check that this watcher's encoder produces the same path
        // shape as the canonical encoder in `transcript::encode_project_path`.
        // We compare against a known-good output rather than calling the
        // private function.
        assert_eq!(
            encode_for_lookup("C:/Users/jspin/Documents/qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
        );
        assert_eq!(
            encode_for_lookup("C:\\Users\\jspin\\Documents\\qontinui_parent"),
            "C--Users-jspin-Documents-qontinui-parent"
        );
    }

    #[test]
    fn test_path_looks_like_workflow_session_missing_file() {
        // Missing files must not blow up; treat as not-workflow and let the
        // in-task re-check decide later.
        let nope = std::path::Path::new("C:/this/path/does/not/exist.jsonl");
        assert!(!path_looks_like_workflow_session(nope));
    }

    #[test]
    fn test_path_looks_like_workflow_session_detects_marker() {
        let tmp =
            std::env::temp_dir().join(format!("qontinui-tw-test-{}.jsonl", std::process::id()));
        std::fs::write(
            &tmp,
            r#"{"type":"system","subtype":"queue-operation","timestamp":"2026-05-09T00:00:00Z"}
{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:01Z","message":{"role":"user","content":"hi"}}
"#,
        )
        .unwrap();
        assert!(path_looks_like_workflow_session(&tmp));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_path_looks_like_workflow_session_interactive_passes() {
        let tmp = std::env::temp_dir().join(format!(
            "qontinui-tw-test-interactive-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &tmp,
            r#"{"type":"user","uuid":"u1","timestamp":"2026-05-09T00:00:01Z","message":{"role":"user","content":"hi"}}
"#,
        )
        .unwrap();
        assert!(!path_looks_like_workflow_session(&tmp));
        let _ = std::fs::remove_file(&tmp);
    }
}
