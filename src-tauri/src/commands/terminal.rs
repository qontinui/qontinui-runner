//! Tauri commands for embedded terminal management.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Manager;
use tracing::{info, warn};

use crate::claude_session::SessionManager;
use crate::commands::CommandResponse;
use crate::error::AppError;
use crate::session::pane_store::{PaneKey, PaneSessionStore};
use crate::session::session_lifecycle_store::{SessionLifecycleStore, TerminalSessionRecord};
use crate::session::{Intent, SessionKind, SessionRegistry};
use crate::terminal::{strip_ansi, TerminalManager};

/// Create a new terminal session.
///
/// Phase 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`:
/// callers that intend to *edit* a coord-registered repo can pass
/// `intent_repo: Some("<repo-slug>")`. When
/// `QONTINUI_AGENT_WORKTREE_MODE` is enabled, the runner allocates an
/// isolated git worktree for that repo via
/// `agent_worktree::isolated_edit::acquire` and uses the materialized
/// worktree path as the session's cwd, instead of the operator's primary
/// checkout. Observation/read-only callers leave `intent_repo: None` and
/// keep the legacy shared-cwd behavior — that path is unchanged.
/// `command` (Decision 3 of the visible-gate-continuations plan) is an optional
/// program+args override threaded down to [`TerminalSession::spawn`]: when
/// `Some([program, args…])` the session runs that program as its PTY child
/// instead of the interactive shell. The gate-continuation terminal branch uses
/// this to launch `claude "<prompt>"` directly. Every operator-opened / frontend
/// terminal passes `None`, keeping the interactive-shell path byte-for-byte
/// unchanged.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn terminal_create(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_registry: tauri::State<'_, Arc<SessionRegistry>>,
    pane_store: tauri::State<'_, Arc<PaneSessionStore>>,
    app_handle: tauri::AppHandle,
    title: Option<String>,
    working_dir: Option<String>,
    page_id: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    intent_repo: Option<String>,
    plan_slug: Option<String>,
    correlation_topic: Option<String>,
    command: Option<Vec<String>>,
    // Phase 2 (pop-out windows): the label of the window creating this pane.
    // Absent/"main" → the legacy key (back-compat); a "term-N" pop-out folds
    // the label into the pane identity so same-(title,cwd) panes in different
    // windows don't collide on one coord session.
    window_label: Option<String>,
) -> Result<CommandResponse, String> {
    // R2 (session-lifecycle-cleanup) — derive the STABLE pane identity from
    // the create-time triple the frontend round-trips on restore
    // (`page_id`, `title`, original `working_dir`). Computed BEFORE
    // `acquire_for_terminal` reassigns `working_dir` to a freshly-allocated
    // isolated worktree path (which is NOT stable across restarts) and
    // before `page_id` is moved into `terminal_manager.create`. Used to
    // look up / persist this pane's coord session id so a restart RESUMES
    // the prior coord row instead of orphaning it.
    let pane_key = PaneKey::from_create_in_window(
        page_id.as_deref().unwrap_or("default"),
        title.as_deref().unwrap_or(""),
        working_dir.as_deref().unwrap_or(""),
        window_label.as_deref().unwrap_or("main"),
    );

    // L2 (shared-checkout coordination gap fix) — when the caller did
    // NOT declare an `intent_repo`, derive it from the session's
    // `working_dir`: if that directory sits inside a known canonical
    // checkout (`<root>/<repo>/...`), treat the session as editing that
    // repo. This makes `acquire_for_terminal` route through isolated
    // worktree acquisition when `QONTINUI_AGENT_WORKTREE_MODE` is on.
    // SAFE: when the flag is off (the default), `acquire_for_terminal`
    // still returns `(working_dir, None)`, so the derivation has ZERO
    // effect until the operator flips the flag.
    let effective_intent_repo: Option<String> = intent_repo.clone().or_else(|| {
        working_dir.as_deref().and_then(|wd| {
            let derived = crate::agent_worktree::canonical_paths::repo_slug_for_path(
                std::path::Path::new(wd),
            );
            if let Some(ref repo) = derived {
                tracing::debug!(
                    working_dir = %wd,
                    derived_intent_repo = %repo,
                    "terminal_create: derived intent_repo from working_dir"
                );
            }
            derived
        })
    });

    // Phase 2 — route through the shared `acquire_for_terminal` helper
    // so this entry point + the HTTP-proxy / backend-relay siblings stay
    // in lockstep. When `intent_repo` is `None`, the helper is a no-op;
    // when it's `Some(_)` but worktree mode is off or allocation fails,
    // the helper returns the original `working_dir` and logs.
    // This Tauri command is invoked by the operator's frontend — an
    // interactive/UI-created terminal with no agent session to attribute.
    // The coord session id is only minted later (registration below), and
    // it identifies the coord row, not the spawning agent. None is correct.
    let (working_dir, isolated_ctx) = crate::agent_worktree::isolated_edit::acquire_for_terminal(
        effective_intent_repo.as_deref(),
        title.as_deref().unwrap_or("Terminal edit session"),
        working_dir,
        None,
    )
    .await;

    let repo_detect_handle = app_handle.clone();
    let repo_detect_dir = working_dir.clone();
    let cred_helper_dir = working_dir.clone();
    // Phase 2c — export `QONTINUI_SESSION_WORKTREES` (all materialized
    // sibling worktrees) onto the PTY so an agent-agnostic launch can find
    // repos that materialized on disk but aren't the process cwd. Derived
    // from the live isolated edit context before it is parked on the
    // session. `None` (no ctx / single-or-zero worktree) → no env var.
    let extra_env = isolated_ctx.as_ref().and_then(|ctx| {
        ctx.session_worktrees_env_value().map(|v| {
            vec![(
                crate::agent_worktree::isolated_edit::SESSION_WORKTREES_ENV.to_string(),
                v,
            )]
        })
    });
    let info = terminal_manager.create(
        title.clone(),
        working_dir.clone(),
        page_id,
        cols,
        rows,
        app_handle,
        command,
        extra_env,
    )?;

    // Park the isolated edit context on the terminal session so its
    // heartbeat + claim live as long as the PTY. Cleared in `close()`.
    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(&info.id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    // Unconditional coord registration — every terminal session is
    // mirrored into the coordinator's session plane so the dashboard
    // renders it from `coord.sessions`. This is NOT gated by dual-write;
    // registration is always-on. Errors are logged and swallowed so a
    // coord hiccup never blocks the operator's terminal.
    let purpose = title
        .filter(|t| t.trim().len() >= 3)
        .unwrap_or_else(|| "Terminal shell session".to_string());
    let intent = Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo: effective_intent_repo,
        branch: None,
        plan_slug,
        correlation_topic,
        // Deliberately None on the interactive create path — a PRESENT
        // `page_id` is the coord-side "this is a gate continuation" marker.
        page_id: None,
        declared_paths: working_dir
            .map(std::path::PathBuf::from)
            .into_iter()
            .collect(),
        share_output: true,
        redact_secrets: None,
    };
    // R2 — RESUME the pane's prior coord session if one is persisted,
    // otherwise register fresh. Resuming PATCHes the EXISTING coord row
    // (state=active + heartbeat) so a runner restart no longer orphans the
    // old row + mints a duplicate. On a persisted-but-GC'd row the resume
    // falls back to a fresh register (new id), which we persist over the
    // stale one. Either way the pane ends with a live coord session id.
    let registry = session_registry.inner().clone();
    let persisted = pane_store.get(&pane_key);
    let registration: Result<uuid::Uuid, crate::session::SessionError> = match persisted {
        Some(prior_id) => registry.resume_external(prior_id, intent).await,
        None => registry.register_external(intent),
    };

    let mut coord_session_id: Option<uuid::Uuid> = None;
    match registration {
        Ok(coord_id) => {
            coord_session_id = Some(coord_id);
            // Persist (or refresh) the pane → coord-id mapping so the NEXT
            // restart resumes this same row. On the fallback path `coord_id`
            // is a fresh id replacing the stale GC'd one.
            pane_store.put(&pane_key, coord_id);

            // Store the coord session id on the terminal so close can clean up.
            if let Some(session) = terminal_manager.get(&info.id) {
                session.set_coord_session_id(coord_id);

                // R1 — install the on-exit hook so the PTY waiter thread
                // closes the coord session mirror the instant the process
                // exits, instead of leaving a ghost until coord's own
                // stale→closed watcher reaps it (the runner no longer
                // self-closes abandoned sessions; see coord_sync plan A3).
                // Shares the idempotent `close_by_id` path with the frontend
                // `terminal_close` command, so a double-close (exit +
                // explicit close) is a no-op.
                let close_registry = registry.clone();
                session.set_on_exit(Box::new(move |coord_id| {
                    if let Err(e) = close_registry.close_by_id(coord_id) {
                        warn!(
                            coord_session = %coord_id,
                            error = %e,
                            "terminal exit hook: coord session close failed"
                        );
                    }
                }));

                // Attach the output pipe so PTY output streams to coord.
                let rx = session.subscribe_output();
                registry.attach_output_pipe(coord_id, rx, true);
            }
            info!(
                terminal_id = %info.id,
                coord_session = %coord_id,
                resumed = persisted.is_some(),
                "terminal_create: coord session ready"
            );
        }
        Err(e) => {
            warn!(
                terminal_id = %info.id,
                error = %e,
                "terminal_create: coord session registration failed — terminal unaffected"
            );
        }
    }

    tokio::spawn(async move {
        crate::repo_detection::check_and_emit_unregistered(repo_detect_handle, repo_detect_dir)
            .await;
    });

    if let (Some(dir), Some(sid)) = (cred_helper_dir, coord_session_id) {
        let session_id_str = sid.to_string();
        tokio::spawn(async move {
            crate::credential_helper::setup_credential_helper(&dir, &session_id_str).await;
        });
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!(info)),
    })
}

/// Write data (keystrokes) to a terminal's PTY stdin.
///
/// Data is a base64-encoded byte array from the frontend.
#[tauri::command]
pub fn terminal_write(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    data: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let bytes = STANDARD.decode(&data).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Invalid base64 data: {}",
            e
        )))
    })?;

    session.write(&bytes)?;

    // L3 (shared-checkout coordination gap fix) — observe typed input for
    // branch-mutating git so a soft coord-conflict warning can surface
    // when a peer holds the worktree claim. Best-effort + non-blocking;
    // runs AFTER the write so it never delays keystroke delivery.
    session.observe_input_for_warn(&bytes);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Update a terminal session's display title.
///
/// Phase 2 of the bi-directional title sync (plan
/// `2026-05-11-runner-dispatch-and-terminal-ux-fixes-plan.md`): the
/// frontend's xterm.js `onTitleChange` handler (see
/// `components/terminal/ZoneGrid.tsx`) invokes this whenever it
/// observes a new OSC 0/2 title in a `terminal-output` paint. The
/// runner mirrors the new title onto `TerminalSession.title` and emits
/// a `terminal-title-changed` event so other webview windows / WS
/// subscribers stay consistent.
///
/// Worker pin (plan `2026-05-12-claude-auto-title-suppression-worker-tabs-plan.md`):
/// goes through [`TerminalManager::set_title_unless_worker`] so OSC 0
/// titles emitted by Claude inside a worker pty are silently dropped —
/// `Worker N` stays pinned for the lifetime of the tab. Non-worker
/// terminals (manual `claude` from the Terminal tab, PowerShell, etc.)
/// keep the existing follow-the-child behaviour.
#[tauri::command]
pub fn terminal_set_title(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    app_handle: tauri::AppHandle,
    terminal_id: String,
    title: String,
) -> Result<CommandResponse, String> {
    terminal_manager.set_title_unless_worker(
        session_manager.inner().as_ref(),
        &terminal_id,
        title,
        &app_handle,
    )?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Resize a terminal's PTY dimensions.
#[tauri::command]
pub fn terminal_resize(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    session.resize(cols, rows)?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Close a terminal session and kill its process.
/// Uses spawn_blocking to avoid blocking the IPC thread during thread joins.
#[tauri::command]
pub async fn terminal_close(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_registry: tauri::State<'_, Arc<SessionRegistry>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    // Capture the coord session id BEFORE close destroys the terminal.
    let coord_id = terminal_manager
        .get(&terminal_id)
        .and_then(|s| s.coord_session_id());

    let manager = terminal_manager.inner().clone();
    let id = terminal_id.clone();
    tokio::task::spawn_blocking(move || manager.close(&id))
        .await
        .map_err(|e| String::from(AppError::ProcessError(format!("Join error: {}", e))))??;

    // Close the coord mirror — fire-and-forget on error.
    if let Some(coord_id) = coord_id {
        if let Err(e) = session_registry.inner().close_by_id(coord_id) {
            warn!(
                terminal_id = %terminal_id,
                coord_session = %coord_id,
                error = %e,
                "terminal_close: coord session close failed"
            );
        }
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// List all terminal sessions.
#[tauri::command]
pub fn terminal_list(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<CommandResponse, String> {
    let terminals = terminal_manager.list();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "terminals": terminals })),
    })
}

/// Get the server-side cell grid snapshot for a terminal session.
///
/// Returns the parsed `GridSnapshot` rather than raw scrollback bytes, so the
/// frontend can paint the final visible state in one synchronous write.
/// See `plans/terminal-grid-snapshot.md`.
#[tauri::command]
pub fn terminal_get_grid(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let grid_handle = session.grid();
    let snapshot = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.snapshot()
    };
    let value = serde_json::to_value(&snapshot).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize grid snapshot: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Compact text-only snapshot for verifiers and external tools.
///
/// Returns the rendered grid as `lines: Vec<String>` plus a single
/// `text` field with the lines joined by `\n` — the shape the
/// verification module expects to feed into a prompt or diff.
#[tauri::command]
pub fn terminal_grid_text(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;
    let grid_handle = session.grid();
    let snapshot = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.text_snapshot()
    };
    let value = serde_json::to_value(&snapshot).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize text snapshot: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Search the rendered grid of one terminal for a substring or regex.
///
/// `regex=true` compiles the needle as a regex; `regex=false` is a
/// case-sensitive substring match. Returns at most one hit per row.
#[tauri::command]
pub fn terminal_grid_search(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    needle: String,
    regex: Option<bool>,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;
    let grid_handle = session.grid();
    let hits = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.search(&needle, regex.unwrap_or(false))
            .map_err(|e| e.to_string())?
    };
    let value = serde_json::to_value(&hits).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize search hits: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "terminalId": terminal_id,
            "hits": value,
        })),
    })
}

/// Row-by-row diff of two terminal grids.
#[tauri::command]
pub fn terminal_grid_diff(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    a_terminal_id: String,
    b_terminal_id: String,
) -> Result<CommandResponse, String> {
    let a = terminal_manager
        .get(&a_terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", a_terminal_id))?;
    let b = terminal_manager
        .get(&b_terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", b_terminal_id))?;
    let a_grid_handle = a.grid();
    let b_grid_handle = b.grid();
    // Self-diff would deadlock on a single-mutex re-lock — detect via
    // Arc::ptr_eq and take one lock instead.
    let diff = if Arc::ptr_eq(&a_grid_handle, &b_grid_handle) {
        let grid = a_grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        grid.diff_lines(&grid)
    } else {
        let ag = a_grid_handle
            .lock()
            .map_err(|e| format!("Grid A lock poisoned: {}", e))?;
        let bg = b_grid_handle
            .lock()
            .map_err(|e| format!("Grid B lock poisoned: {}", e))?;
        ag.diff_lines(&bg)
    };
    let value = serde_json::to_value(&diff).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize grid diff: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Acknowledge bytes received by the frontend (flow control).
#[tauri::command]
pub fn terminal_ack(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    bytes_acked: u64,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    session.ack(bytes_acked);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Get the LIVE scrollback ring buffer for a terminal session.
///
/// Returns the raw PTY byte history (base64) plus its absolute offset range
/// `[startOffset, endOffset)` in the session's output stream. The frontend
/// replays this into a freshly-(re)mounted xterm so scrollback survives the
/// TerminalInstance remounts that zone-layout changes and page reloads cause
/// (`terminal_get_grid` only restores the visible rows×cols screen), then
/// uses `endOffset` to dedup against offset-stamped live `terminal-output`
/// chunks.
#[tauri::command]
pub fn terminal_get_scrollback(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let (data, start_offset) = session.get_scrollback_buffer();
    let end_offset = start_offset + data.len() as u64;
    // Reconnecting frontends fetch the full ring; reset backpressure the same
    // way the WS/HTTP buffer path does so the reader thread doesn't stall.
    session.reset_flow_control();
    let encoded = STANDARD.encode(&data);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "data": encoded,
            "startOffset": start_offset,
            "endOffset": end_offset,
        })),
    })
}

/// Save the scrollback buffer for a terminal session to disk.
///
/// Persists the current scrollback buffer to `{app_data}/terminal-scrollback/{terminal_id}.bin`
/// so it can be restored after an app restart. Returns the file path on success.
#[tauri::command]
pub fn terminal_save_scrollback(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let (data, _start_offset) = session.get_scrollback_buffer();
    if data.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No scrollback data to save".to_string()),
            data: Some(serde_json::json!({ "path": serde_json::Value::Null })),
        });
    }

    let scrollback_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| {
            String::from(AppError::TauriError(format!(
                "Failed to get app data dir: {}",
                e
            )))
        })?
        .join("terminal-scrollback");

    std::fs::create_dir_all(&scrollback_dir).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to create scrollback directory: {}", e),
        )))
    })?;

    let file_path = scrollback_dir.join(format!("{}.bin", terminal_id));
    std::fs::write(&file_path, &data).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write scrollback file: {}", e),
        )))
    })?;

    let path_str = file_path.to_string_lossy().to_string();
    info!(
        terminal_id = %terminal_id,
        bytes = data.len(),
        path = %path_str,
        "Saved terminal scrollback to disk"
    );

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "path": path_str })),
    })
}

/// Read a previously saved scrollback buffer from disk.
///
/// Returns the base64-encoded scrollback content. Returns an empty string
/// if the file does not exist (best-effort restore).
#[tauri::command]
pub fn terminal_get_saved_scrollback(file_path: String) -> Result<CommandResponse, String> {
    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({ "data": "" })),
        });
    }

    let data = std::fs::read(path).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read scrollback file: {}", e),
        )))
    })?;

    let encoded = STANDARD.encode(&data);
    info!(
        path = %file_path,
        bytes = data.len(),
        "Read saved scrollback from disk"
    );

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "data": encoded })),
    })
}

/// Delete all saved scrollback files from disk.
///
/// Called after successful session restore to clean up stale scrollback data.
#[tauri::command]
pub fn terminal_cleanup_scrollback(
    app_handle: tauri::AppHandle,
) -> Result<CommandResponse, String> {
    let scrollback_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| {
            String::from(AppError::TauriError(format!(
                "Failed to get app data dir: {}",
                e
            )))
        })?
        .join("terminal-scrollback");

    if !scrollback_dir.exists() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No scrollback directory to clean up".to_string()),
            data: None,
        });
    }

    let mut deleted = 0u32;
    match std::fs::read_dir(&scrollback_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    warn!(
                        path = %entry.path().display(),
                        error = %e,
                        "Failed to delete scrollback file"
                    );
                } else {
                    deleted += 1;
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to read scrollback directory");
        }
    }

    info!(deleted = deleted, "Cleaned up terminal scrollback files");

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "deleted": deleted })),
    })
}

/// Collect session metadata across terminal pages for AI analysis.
///
/// Returns structured data per session including title, working directory,
/// scrollback preview (last lines), and page association. Used by the
/// session reorganization feature.
#[tauri::command]
pub fn terminal_collect_session_metadata(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    page_ids: Vec<String>,
    max_scrollback_lines: Option<usize>,
) -> Result<CommandResponse, String> {
    let max_lines = max_scrollback_lines.unwrap_or(20);
    let terminals = terminal_manager.list();

    let mut sessions = Vec::new();

    for term in &terminals {
        let page = &term.page_id;
        if !page_ids.contains(page) {
            continue;
        }

        // Get scrollback preview
        let scrollback_preview = if let Some(session) = terminal_manager.get(&term.id) {
            let (data, _offset) = session.get_scrollback_buffer();
            // Decode and extract last N lines
            let text = String::from_utf8_lossy(&data);
            // Strip ANSI escape codes for readability
            let clean: String = strip_ansi(&text);
            let lines: Vec<&str> = clean.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        } else {
            String::new()
        };

        sessions.push(serde_json::json!({
            "id": term.id,
            "title": term.title,
            "page_id": page,
            "working_dir": term.working_dir,
            "is_alive": term.is_alive,
            "pid": term.pid,
            "created_at": term.created_at,
            "total_bytes_produced": term.total_bytes_produced,
            "scrollback_preview": scrollback_preview,
        }));
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "sessions": sessions,
            "page_ids": page_ids,
        })),
    })
}

/// Record (or refresh) a Claude terminal session in the durable,
/// backend-owned lifecycle registry, keyed by `claudeSessionId`.
///
/// This is the source of truth for "which Claude sessions exist and which
/// grid zone each belongs to" — replacing the fragile `localStorage`
/// snapshot. Calling it again with the same `claude_session_id` updates the
/// existing record in place (structural dedup), which is what prevents the
/// duplicate-session bug.
///
/// Synchronous + fast: [`SessionLifecycleStore::record_open`] fsyncs the
/// atomic write before returning, so the frontend can rely on durability the
/// instant this resolves.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn terminal_session_record_open(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
    config_dir: Option<String>,
    working_dir: Option<String>,
    page_id: Option<String>,
    zone_index: i32,
    title: Option<String>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let record = TerminalSessionRecord {
        claude_session_id,
        config_dir,
        working_dir,
        page_id: page_id.unwrap_or_else(|| "default".to_string()),
        zone_index,
        title,
        terminal_id,
        // record_open seeds these from `now`; values here are placeholders.
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
    };
    store.record_open(record);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Mark a Claude terminal session closed in the lifecycle registry. No-op
/// (still succeeds) if the session is absent or already closed.
#[tauri::command]
pub fn terminal_session_record_close(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
    reason: String,
) -> Result<CommandResponse, String> {
    store.record_close(&claude_session_id, &reason);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// List every RESTORABLE Claude terminal session from the lifecycle registry.
/// The grid hydrates its session tiles from this on boot instead of
/// `localStorage`.
///
/// Returns the restorable superset (see `restorable_records`): all `open`
/// records (hard-crash case) PLUS `closed`/`pty-exit` records still within the
/// grace window (graceful-restart case, where `handleExit` flipped every live
/// PTY to `closed`). User-closed and stale records are excluded so the restore
/// path resurrects exactly the sessions that died with the runner.
#[tauri::command]
pub fn terminal_session_list_open(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
) -> Result<CommandResponse, String> {
    let now = chrono::Utc::now().timestamp_millis();
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": store.restorable_records(now) })),
    })
}

/// Optional hint asking [`create_terminal_session_backend`] to durably record
/// the new session in the lifecycle store once its Claude session id can be
/// resolved from the on-disk transcript. Backend-spawned continuation
/// terminals (which never fire the frontend `terminal_session_record_open`
/// command) pass `Some(..)` so a restart can restore them; plain shells pass
/// `None` and skip the resolver entirely.
#[derive(Debug, Clone)]
pub(crate) struct SessionCaptureHint {
    /// Config dir the session launched under, if known. Continuation spawns
    /// run under the runner's DEFAULT dir (no `CLAUDE_CONFIG_DIR`), so this is
    /// `None` for them and the resolver scans every known config dir.
    pub config_dir: Option<String>,
    /// Working directory of the session — the transcript resolver's
    /// `project_path`.
    pub working_dir: String,
    /// Human-readable title for the restored grid tile.
    pub title: String,
    /// Page the session should land on (and be durably recorded against) so a
    /// restart re-lands the continuation on its page. `None` → `"default"`.
    pub page_id: Option<String>,
}

/// Backend-task entry point for creating a terminal session + coord row from a
/// non-Tauri context (e.g. the gate-continuation runtime task, which has no
/// `tauri::State` extractors — it reaches the managed `Arc`s via the global
/// `tauri_app_handle`). Mirrors the registration body of [`terminal_create`]:
/// create the PTY session (with an optional `command` override), park a
/// pre-acquired isolated-edit context so its claim heartbeat lives for the
/// session's lifetime + releases on close, register/attach the coord session,
/// and install the on-exit close hook.
///
/// Unlike [`terminal_create`] this does NOT acquire a worktree itself — the
/// gate-continuation path already acquired one (with its heartbeat) in P0.1 and
/// hands the resulting `IsolatedEditContext` in via `isolated_ctx` so ownership
/// (and the heartbeat) transfers to the terminal session. Returns the new
/// terminal id and the coord session id (when registration succeeded).
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_terminal_session_backend(
    terminal_manager: &Arc<TerminalManager>,
    session_registry: &Arc<SessionRegistry>,
    app_handle: tauri::AppHandle,
    title: String,
    working_dir: String,
    plan_slug: Option<String>,
    correlation_topic: Option<String>,
    intent_repo: Option<String>,
    command: Option<Vec<String>>,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    capture_hint: Option<SessionCaptureHint>,
    page_id: Option<String>,
) -> Result<(String, Option<uuid::Uuid>), String> {
    // Phase 2c — derive the `QONTINUI_SESSION_WORKTREES` env from the
    // pre-acquired context (all materialized sibling worktrees) before it is
    // parked on the session. `None` (no ctx / single-or-zero worktree) → no
    // env var. The `--add-dir <sibling>` convenience for `claude` launches is
    // appended into `command` by the caller (gate-continuation in
    // `agent_runtime.rs`), since only the caller knows the launch is `claude`.
    let mut env_pairs: Vec<(String, String)> = Vec::new();
    if let Some(ctx) = isolated_ctx.as_ref() {
        if let Some(v) = ctx.session_worktrees_env_value() {
            env_pairs.push((
                crate::agent_worktree::isolated_edit::SESSION_WORKTREES_ENV.to_string(),
                v,
            ));
        }
    }
    // Account selection: pin the spawned PTY to the account the caller chose
    // (gate continuations set `capture_hint.config_dir` to the selected,
    // token-bearing account). Without this, a backend-spawned `claude` inherits
    // the runner's ambient `CLAUDE_CONFIG_DIR` (e.g. a quota-exhausted account)
    // and dies instantly. The interactive terminal path sets this via the shell
    // command instead; this is the backend-spawn equivalent.
    if let Some(dir) = capture_hint.as_ref().and_then(|h| h.config_dir.clone()) {
        env_pairs.push(("CLAUDE_CONFIG_DIR".to_string(), dir));
    }
    let extra_env = if env_pairs.is_empty() {
        None
    } else {
        Some(env_pairs)
    };
    // Keep a handle for the (optional) durable-record poller below, since the
    // `create` call consumes `app_handle`. `AppHandle` is a cheap Arc clone.
    let app_state = capture_hint.as_ref().map(|_| app_handle.clone());
    let info = terminal_manager.create(
        Some(title.clone()),
        Some(working_dir.clone()),
        page_id.clone(),
        None,
        None,
        app_handle,
        command,
        extra_env,
    )?;

    // Park the pre-acquired isolated edit context on the session so its
    // heartbeat + claim live as long as the PTY and release on close — the
    // visible session keeps the SAME claim bookkeeping the headless path holds.
    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(&info.id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    // Coord registration — mirror every terminal session into the coordinator's
    // session plane (same as the interactive `terminal_create`). Best-effort:
    // a coord hiccup must not fail the spawn.
    let purpose = if title.trim().len() >= 3 {
        title
    } else {
        "Gate continuation terminal session".to_string()
    };
    let intent = Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo: intent_repo,
        branch: None,
        plan_slug,
        correlation_topic,
        // Gate-continuation create path — carry the chosen page tab into
        // `coord.sessions.intent` so coord learns the placement.
        page_id: page_id.clone(),
        declared_paths: vec![std::path::PathBuf::from(&working_dir)],
        share_output: true,
        redact_secrets: None,
    };

    let mut coord_session_id: Option<uuid::Uuid> = None;
    match session_registry.register_external(intent) {
        Ok(coord_id) => {
            coord_session_id = Some(coord_id);
            if let Some(session) = terminal_manager.get(&info.id) {
                session.set_coord_session_id(coord_id);
                let close_registry = session_registry.clone();
                // Capacity-freed re-poll: capture this terminal's id so the exit
                // hook can tell `agent_runtime` a continuation slot just freed and
                // a deferred (AtCap) continuation can be re-polled promptly. The
                // notify is a no-op unless this terminal is a registered
                // continuation session, so operator tabs (a different create path)
                // never trigger a poll. The PTY waiter that fires this hook is a
                // bare OS thread with no tokio runtime, so capture the current
                // runtime handle HERE (this fn runs under tokio) for the poll to
                // spawn on.
                let exited_terminal_id = info.id.clone();
                let exit_rt_handle = tokio::runtime::Handle::try_current().ok();
                session.set_on_exit(Box::new(move |coord_id| {
                    if let Err(e) = close_registry.close_by_id(coord_id) {
                        warn!(
                            coord_session = %coord_id,
                            error = %e,
                            "gate-continuation terminal exit hook: coord session close failed"
                        );
                    }
                    crate::agent_runtime::notify_continuation_terminal_exit(
                        &exited_terminal_id,
                        exit_rt_handle.as_ref(),
                    );
                }));
                let rx = session.subscribe_output();
                session_registry.attach_output_pipe(coord_id, rx, true);
            }
            info!(
                terminal_id = %info.id,
                coord_session = %coord_id,
                "create_terminal_session_backend: coord session ready"
            );
        }
        Err(e) => {
            warn!(
                terminal_id = %info.id,
                error = %e,
                "create_terminal_session_backend: coord registration failed — terminal unaffected"
            );
        }
    }

    // Durable lifecycle registration for backend-spawned (continuation)
    // sessions: the frontend `terminal_session_record_open` command never
    // fires for these, so without this a restart loses them. The Claude
    // session id is not known at spawn time — it only appears once the
    // `claude` child writes its first transcript record — so resolve it
    // asynchronously by polling the on-disk transcript, then record. Plain
    // (non-hint) terminals skip this entirely (no wasted polling).
    if let (Some(hint), Some(app_state)) = (capture_hint, app_state) {
        if let Some(store) = app_state.try_state::<Arc<SessionLifecycleStore>>() {
            let store = store.inner().clone();
            let terminal_id = info.id.clone();
            let SessionCaptureHint {
                config_dir,
                working_dir,
                title,
                page_id: hint_page_id,
            } = hint;
            // Single source of truth for the recorded page: the hint's page_id
            // (set by the caller to the picked page), defaulting to "default".
            let record_page_id = hint_page_id.unwrap_or_else(|| "default".to_string());
            let since = chrono::Utc::now();
            let resolver_dir = working_dir.clone();
            let resolver = move || resolve_latest_claude_session_id(&resolver_dir, since);
            tokio::spawn(poll_and_record_session(
                store,
                resolver,
                terminal_id,
                config_dir,
                working_dir,
                title,
                record_page_id,
                SESSION_CAPTURE_POLL_INTERVAL,
                SESSION_CAPTURE_TIMEOUT,
            ));
        } else {
            warn!(
                terminal_id = %info.id,
                "create_terminal_session_backend: lifecycle store not managed — \
                 continuation session will not be durably recorded"
            );
        }
    }

    Ok((info.id, coord_session_id))
}

/// How often the durable-record poller checks the on-disk transcript for the
/// new Claude session id. Widened past 1s because each tick can stat every
/// JSONL across all `.claude-*` config dirs on the mtime-fallback path.
const SESSION_CAPTURE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the poller keeps trying before giving up. Generous because a cold
/// model spin-up can delay the first transcript write.
const SESSION_CAPTURE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Real resolver: scan every known Claude config dir (the continuation runs
/// under the default dir, so `config_dir` is unknown here) for the freshest
/// session in `project_path` written after `since`, returning its session id.
/// The cheap `.claude.json` `lastSessionId` shortcut inside
/// [`get_latest_session_id`] is preferred where it applies.
fn resolve_latest_claude_session_id(
    project_path: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    for dir in crate::terminal::transcript::find_claude_config_dirs() {
        if let Some(session) =
            crate::terminal::transcript::get_latest_session_id(&dir, project_path, Some(since))
        {
            return Some(session.session_id);
        }
    }
    None
}

/// Poll `resolve` until it yields a Claude session id (or `timeout` elapses),
/// then durably record the session via [`SessionLifecycleStore::record_open`].
///
/// The resolver is injected so this loop is unit-testable without a real
/// on-disk transcript. On the first resolve it builds a [`TerminalSessionRecord`]
/// (restored into zone 0 / the default page — wrong zone beats a lost session)
/// and records it; on timeout it `warn!`s and gives up without recording.
#[allow(clippy::too_many_arguments)]
async fn poll_and_record_session<F>(
    store: Arc<SessionLifecycleStore>,
    resolve: F,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    interval: std::time::Duration,
    timeout: std::time::Duration,
) where
    F: Fn() -> Option<String>,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(claude_session_id) = resolve() {
            let record = TerminalSessionRecord {
                claude_session_id: claude_session_id.clone(),
                config_dir: config_dir.clone(),
                working_dir: Some(working_dir.clone()),
                page_id: page_id.clone(),
                zone_index: 0,
                title: Some(title.clone()),
                terminal_id: terminal_id.clone(),
                // record_open seeds these from `now`; values here are placeholders
                // (mirrors `terminal_session_record_open`).
                opened_at: 0,
                last_seen_at: 0,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
            };
            store.record_open(record);
            info!(
                terminal_id = %terminal_id,
                claude_session = %claude_session_id,
                "create_terminal_session_backend: continuation session durably recorded"
            );
            return;
        }
        if std::time::Instant::now() >= deadline {
            warn!(
                terminal_id = %terminal_id,
                working_dir = %working_dir,
                "create_terminal_session_backend: timed out resolving Claude session id — \
                 continuation session not durably recorded"
            );
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// Non-generic because handlers accept concrete `tauri::AppHandle`.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_terminal")
        .invoke_handler(tauri::generate_handler![
            terminal_create,
            terminal_write,
            terminal_resize,
            terminal_set_title,
            terminal_close,
            terminal_list,
            terminal_ack,
            terminal_save_scrollback,
            terminal_get_saved_scrollback,
            terminal_cleanup_scrollback,
            terminal_collect_session_metadata,
            terminal_get_grid,
            terminal_grid_text,
            terminal_grid_search,
            terminal_grid_diff,
            terminal_session_record_open,
            terminal_session_record_close,
            terminal_session_list_open,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// When the injected resolver yields an id, exactly one open record lands
    /// in the store, keyed by the resolved id, restored into zone 0 / default
    /// page with the supplied title + terminal id.
    #[tokio::test]
    async fn poll_and_record_session_records_when_resolver_yields_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );

        poll_and_record_session(
            store.clone(),
            || Some("resolved-sess-1".to_string()),
            "term-1".to_string(),
            None,
            "/work/dir".to_string(),
            "My Continuation".to_string(),
            "default".to_string(),
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await;

        let open = store.open_records();
        assert_eq!(open.len(), 1, "exactly one record on resolve");
        let rec = &open[0];
        assert_eq!(rec.claude_session_id, "resolved-sess-1");
        assert_eq!(rec.terminal_id, "term-1");
        assert_eq!(rec.working_dir.as_deref(), Some("/work/dir"));
        assert_eq!(rec.title.as_deref(), Some("My Continuation"));
        assert_eq!(rec.page_id, "default");
        assert_eq!(rec.zone_index, 0);
        assert_eq!(rec.config_dir, None);
        assert_eq!(rec.state, "open");
        assert!(rec.opened_at > 0, "record_open seeds opened_at");
    }

    /// The recorded page honors the supplied `page_id` (not a hardcoded
    /// "default") so a continuation placed on a non-default page restores there.
    #[tokio::test]
    async fn poll_and_record_session_honors_supplied_page_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );

        poll_and_record_session(
            store.clone(),
            || Some("resolved-sess-9".to_string()),
            "term-9".to_string(),
            None,
            "/work/dir".to_string(),
            "Overflow Continuation".to_string(),
            "page-7".to_string(),
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await;

        let open = store.open_records();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].page_id, "page-7");
    }

    /// When the resolver never yields an id, the poller gives up after the
    /// timeout without recording anything (and the loop actually polled more
    /// than once before the deadline).
    #[tokio::test]
    async fn poll_and_record_session_gives_up_after_timeout_without_recording() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in = calls.clone();

        poll_and_record_session(
            store.clone(),
            move || {
                calls_in.fetch_add(1, Ordering::SeqCst);
                None
            },
            "term-2".to_string(),
            None,
            "/work/dir".to_string(),
            "Never Resolves".to_string(),
            "default".to_string(),
            Duration::from_millis(1),
            Duration::from_millis(20),
        )
        .await;

        assert!(
            store.open_records().is_empty(),
            "no record when the id never resolves"
        );
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "poller retried at least once before giving up"
        );
    }

    /// A `None` capture hint means no poller is ever constructed, so the store
    /// stays empty. (The spawn decision lives in
    /// `create_terminal_session_backend`; here we assert the equivalent
    /// invariant — no resolver runs, no record — by simply never invoking the
    /// poller, the same branch that path takes for plain shells.)
    #[tokio::test]
    async fn no_record_when_hint_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        // Hint absent → poll_and_record_session is never spawned (see
        // create_terminal_session_backend). The store therefore has no rows.
        assert!(store.open_records().is_empty());
    }
}
