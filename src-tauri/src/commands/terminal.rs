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
#[tauri::command]
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
) -> Result<CommandResponse, String> {
    // R2 (session-lifecycle-cleanup) — derive the STABLE pane identity from
    // the create-time triple the frontend round-trips on restore
    // (`page_id`, `title`, original `working_dir`). Computed BEFORE
    // `acquire_for_terminal` reassigns `working_dir` to a freshly-allocated
    // isolated worktree path (which is NOT stable across restarts) and
    // before `page_id` is moved into `terminal_manager.create`. Used to
    // look up / persist this pane's coord session id so a restart RESUMES
    // the prior coord row instead of orphaning it.
    let pane_key = PaneKey::from_create(
        page_id.as_deref().unwrap_or("default"),
        title.as_deref().unwrap_or(""),
        working_dir.as_deref().unwrap_or(""),
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
    let info = terminal_manager.create(
        title.clone(),
        working_dir.clone(),
        page_id,
        cols,
        rows,
        app_handle,
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

/// List every currently-open Claude terminal session from the lifecycle
/// registry. The grid hydrates its session tiles from this instead of
/// `localStorage`.
#[tauri::command]
pub fn terminal_session_list_open(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
) -> Result<CommandResponse, String> {
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": store.open_records() })),
    })
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
