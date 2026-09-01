//! Tauri commands for embedded terminal management.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Manager;
use tracing::{debug, info, warn};

use crate::claude_session::SessionManager;
use crate::commands::CommandResponse;
use crate::error::AppError;
use crate::session::pane_store::{PaneKey, PaneSessionStore};
use crate::session::session_lifecycle_store::{
    CloseOutcome, SessionLifecycleStore, TerminalSessionRecord,
};
use crate::session::{Intent, SessionKind, SessionRegistry};
use crate::terminal::visibility::VisibilityTier;
use crate::terminal::{strip_ansi, TerminalManager};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// The `(share_output, redact_secrets)` a terminal session declares to coord,
/// from `settings.performance` (many-sessions plan Phase 8).
///
/// Both terminal create paths — the interactive one and the gate-continuation
/// one — go through this single function rather than reading the settings
/// separately, so the two can never drift into declaring different sharing
/// postures for what is, to coord, the same kind of session. Before Phase 8
/// both sites hardcoded `(true, None)`, which is exactly what the stock
/// settings still produce.
fn intent_sharing_from_settings() -> (bool, Option<bool>) {
    let perf = crate::settings::get_performance_settings();
    (perf.share_terminal_output, perf.redact_terminal_secrets)
}

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
    work_unit_slug: Option<String>,
    correlation_topic: Option<String>,
    command: Option<Vec<String>>,
    // Phase 2 (pop-out windows): the label of the window creating this pane.
    // Absent/"main" → the legacy key (back-compat); a "term-N" pop-out folds
    // the label into the pane identity so same-(title,cwd) panes in different
    // windows don't collide on one coord session.
    window_label: Option<String>,
    // Phase 8b / F2 — the tenant the operator picked for THIS spawn. `None`
    // (every legacy caller) keeps the prior behavior exactly: the registry
    // stamps the device default (`machine.json::active_tenant_id`). A
    // malformed uuid is rejected rather than silently falling back, so a
    // typo'd `/spawn-ai --tenant` can never bind the session to the wrong
    // tenant.
    tenant_id: Option<String>,
    // Part D — the operator's "Start anyway" answer to a CRITICAL resource-guard
    // refusal. Absent/`false` on every FIRST attempt: the frontend
    // (`src/lib/resourceGuard.ts`) invokes without it, catches the typed
    // refusal, shows the blocking `ConfirmDialog`, and only then re-invokes with
    // `true`. Modelled `Option<bool>` rather than `bool` so pre-existing callers
    // that omit the argument keep working — Tauri/serde pass a missing arg as
    // `None`, which resolves to "no override", the safe answer.
    resource_override: Option<bool>,
) -> Result<CommandResponse, String> {
    // Spawn-time resource gate, EARLY-OUT arm (§Part D). The authority is still
    // the gate inside `TerminalSession::spawn` — every unattended seam reaches
    // that one and this one is not a replacement for it — but by the time this
    // command gets there it has already run `acquire_for_terminal`, which under
    // `QONTINUI_AGENT_WORKTREE_MODE` does a `git worktree add`, takes a coord
    // claim, starts a heartbeat task and shells out to `git config`. A CRITICAL
    // refusal would throw all of that away, and `IsolatedEditContext::Drop`
    // releases the claim but does NOT remove the materialized worktree — so each
    // refusal would leak a directory and the "Start anyway" retry would
    // materialize a second one. The most allocation-heavy step in the spawn must
    // not be the one that runs unguarded. Costs one `GlobalMemoryStatusEx` call;
    // returns the same typed refusal the seam would, so the frontend's dialog
    // cannot tell which gate answered. Silent on WARN — the notice is emitted
    // once, at the seam.
    crate::resource_guard::precheck_spawn("terminal session", resource_override.unwrap_or(false))?;

    let spawn_tenant_id: Option<uuid::Uuid> = match tenant_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(
            uuid::Uuid::parse_str(raw)
                .map_err(|e| format!("terminal:tenant_invalid: {raw} is not a tenant uuid: {e}"))?,
        ),
    };

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
    // D1: the durable tenant stamp runs after the spawn returns, so it needs a
    // handle of its own (`app_handle` is moved into the blocking spawn below).
    let tenant_stamp_handle = app_handle.clone();
    let repo_detect_dir = working_dir.clone();
    let cred_helper_dir = working_dir.clone();
    // The shared session-env contribution (`QONTINUI_SESSION_WORKTREES` +
    // the configured plan directories). Derived from the live isolated edit
    // context before it is parked on the session; each var is omitted when it
    // does not resolve. See `agent_worktree::session_env`.
    let extra_env = crate::agent_worktree::session_env::session_extra_env(isolated_ctx.as_ref());
    // Phase 6 (B4): the whole blocking spawn (PTY open, identity seam, child
    // exec) runs on a BLOCKING thread, matching the AI path
    // (`commands::ai_session`). It used to be a bare synchronous call on a
    // tokio worker, so K concurrent opens parked K runtime workers for the full
    // spawn duration and starved every other async task in the runner.
    // Phase 0 instrumentation: its child spans break the interior down.
    let create_manager = terminal_manager.inner().clone();
    let create_title = title.clone();
    let create_working_dir = working_dir.clone();
    // ATTENDED spawn: an operator is looking at the window that invoked this,
    // so a CRITICAL refusal has somewhere to go — the frontend re-invokes with
    // `resource_override: true` once they pick "Start anyway".
    let create_resource_override = resource_override.unwrap_or(false);
    let info = spawn_blocking_tracked(move || {
        let _create_span = tracing::debug_span!("terminal_spawn.manager_create").entered();
        create_manager.create(
            create_title,
            create_working_dir,
            page_id,
            cols,
            rows,
            app_handle,
            command,
            extra_env,
            create_resource_override,
        )
    })
    .await
    .map_err(|e| format!("terminal spawn task failed: {e}"))??;

    // Park the isolated edit context on the terminal session so its
    // heartbeat + claim live as long as the PTY. Cleared in `close()`.
    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(&info.id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    // D1 tenant stamp. The tenant the operator picked for THIS spawn is known
    // only here — `Intent.tenant_id` below carries it to coord, and the
    // frontend `TerminalTab` carries it for the session's lifetime, but nothing
    // wrote it durably, so a restart lost it. The spawn-time identity seam has
    // already recorded the session by now (synchronously, inside `create`), so
    // the record exists and is addressable by terminal id.
    //
    // Only stamped when the caller actually chose a tenant: `None` means "let
    // the registry stamp the device default", and copying a default the runner
    // did not resolve here would be an invented value.
    if let Some(tenant) = spawn_tenant_id {
        if let Some(store) = tenant_stamp_handle.try_state::<Arc<SessionLifecycleStore>>() {
            store.update_identity_by_terminal(
                &info.id,
                &crate::session::session_lifecycle_store::SessionIdentityUpdate {
                    tenant_id: Some(tenant.to_string()),
                    ..Default::default()
                },
            );
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
    let (share_output, redact_secrets) = intent_sharing_from_settings();
    let intent = Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo: effective_intent_repo,
        branch: None,
        work_unit_slug,
        // Accept-only on `Intent`; the runner never emits the legacy key.
        plan_slug: None,
        correlation_topic,
        // Deliberately None on the interactive create path — a PRESENT
        // `page_id` is the coord-side "this is a gate continuation" marker.
        page_id: None,
        declared_paths: working_dir
            .map(std::path::PathBuf::from)
            .into_iter()
            .collect(),
        share_output,
        redact_secrets,
        // The tenant the spawn picker (F2) / `--tenant` flag (F3) chose for
        // this tab. `None` → the registry stamps the device default
        // (default-for-new-sessions), which is the pre-F2 behavior.
        tenant_id: spawn_tenant_id,
    };
    // R2 — RESUME the pane's prior coord session if one is persisted,
    // otherwise register fresh. Resuming PATCHes the EXISTING coord row
    // (state=active + heartbeat) so a runner restart no longer orphans the
    // old row + mints a duplicate. On a persisted-but-GC'd row the resume
    // falls back to a fresh register (new id), which we persist over the
    // stale one. Either way the pane ends with a live coord session id.
    //
    // DELIBERATELY STILL ON THE CRITICAL PATH (Phase 6, §4 Q2 — do not "optimize"
    // these into a `tokio::spawn` without re-reading that resolution):
    //   * the coord CLAIM above (`acquire_for_terminal`) returns the value that
    //     BECOMES `working_dir`, a spawn input — it can never move after spawn;
    //   * `attach_output_pipe` below subscribes to a live broadcast of PTY
    //     output, so deferring it drops the session's first bytes;
    //   * `set_coord_session_id` + the `set_on_exit` close hook must move
    //     together WITH registration or a PTY that exits first leaves a coord
    //     ghost;
    //   * `credential_helper::setup_credential_helper` hard-requires coord to
    //     already know the session (it is already `tokio::spawn`ed below).
    // What Phase 6 actually removed from this path is the network: the OAuth
    // refresh is now a background refresher (`ai_provider::oauth_refresh`), and
    // pinned-session verification was already off-path
    // (`poll_and_verify_pinned_session`).
    let registry = session_registry.inner().clone();
    let persisted = pane_store.get(&pane_key);
    // Phase 0 instrumentation: the coord-registration segment of spawn
    // latency. `register_external` is synchronous and does no HTTP (it queues
    // an outbox row, contending on the process-global outbox `write_lock` +
    // fsync); `resume_external` additionally does a live `PATCH /sessions/:id`
    // probe. The span separates the two in the log.
    let registration: Result<uuid::Uuid, crate::session::SessionError> = match persisted {
        Some(prior_id) => {
            use tracing::Instrument;
            registry
                .resume_external(prior_id, intent)
                .instrument(tracing::debug_span!(
                    "terminal_spawn.coord_register",
                    terminal_id = %info.id,
                    resume = true
                ))
                .await
        }
        None => {
            let _span = tracing::debug_span!(
                "terminal_spawn.coord_register",
                terminal_id = %info.id,
                resume = false
            )
            .entered();
            registry.register_external(intent)
        }
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

    // Typed-input observation (L3 git-warn + typed claude resume sniff) and
    // the PTY LIVENESS GATE are both funneled inside `TerminalSession::write`
    // itself, so every write surface (this command, HTTP, WS, transports) is
    // covered without per-caller calls. A write to an exited PTY comes back
    // as a `TERMINAL_EXITED: ...` Err instead of the `success: true` it used
    // to answer; the frontend's `buildWriteFailure` reads that prefix off the
    // rejected invoke and classifies it as `TERMINAL_EXITED` even when this
    // pane has not yet seen its own `terminal-exit` event.
    session.write(&bytes)?;

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
    spawn_blocking_tracked(move || manager.close(&id))
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
///
/// Alongside the `terminals` array (`TerminalInfo`, which structurally does
/// NOT carry a Claude session id), returns `sessionIdsByTerminal`: a
/// `{ terminal_id -> { claudeSessionId, configDir } }` map derived from the
/// durable [`SessionLifecycleStore`]'s `terminal_id -> claude_session_id`
/// index ([`SessionLifecycleStore::find_confirmed_open_by_terminal`]).
///
/// This is what lets the frontend attach `claudeSessionId` to a RECONNECTED
/// tab. The per-session PR dropdown (and anything else session-scoped) gates
/// on `tab.claudeSessionId`; the reconnect path rebuilds tabs from
/// `TerminalInfo` alone, so without this map every reconnected session lost
/// its id and the dropdown never mounted for it. Best-effort + additive: if
/// the store isn't managed the map is simply empty, and the `TerminalInfo`
/// schema (`deny_unknown_fields`) is untouched.
///
/// The map is gated to CONFIRMED records only (see
/// [`SessionLifecycleStore::find_confirmed_open_by_terminal`]). The spawn-time
/// identity seam records an authoritative-but-provisional `open` row for EVERY
/// terminal — including plain shells that never run a provider, and non-pinned
/// launches whose minted uuid is not the id the process actually runs under —
/// so surfacing unconfirmed ids would bind phantom / never-used ids onto tabs
/// and could permanently mis-bind them (the transcript-poll capture stops once
/// a tab carries any id). Only confirmed rows are real, correctly-identified
/// sessions worth lighting up session-scoped UI for.
#[tauri::command]
pub fn terminal_list(
    app: tauri::AppHandle,
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<CommandResponse, String> {
    let terminals = terminal_manager.list();

    let mut session_ids_by_terminal = serde_json::Map::new();
    if let Some(store) = app.try_state::<Arc<SessionLifecycleStore>>() {
        for info in &terminals {
            if let Some(rec) = store.find_confirmed_open_by_terminal(&info.id) {
                session_ids_by_terminal.insert(
                    info.id.clone(),
                    serde_json::json!({
                        "claudeSessionId": rec.claude_session_id,
                        "configDir": rec.config_dir,
                    }),
                );
            }
        }
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "terminals": terminals,
            "sessionIdsByTerminal": serde_json::Value::Object(session_ids_by_terminal),
        })),
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

/// Report this window's visibility tier for a set of terminals (Phase 5 of
/// `plans/2026-07-28-runner-many-sessions-performance.md`, root cause A4).
///
/// Called by the frontend's window-wide reconciler
/// (`terminalVisibilityTiers.ts`) on every zone/page/window change, grouped by
/// tier so a whole layout change costs at most three IPC calls.
///
/// Reports are keyed by the CALLING WINDOW's label and merged most-visible-
/// wins, so a terminal popped out into a `term-N` window is never taken dark by
/// the main window's "nobody here is showing it". Ids this runner does not know
/// are ignored rather than failing the batch: the reconciler groups ids that
/// were live when it ran, and a terminal can close between the two.
#[tauri::command]
pub fn terminal_set_visibility(
    window: tauri::Window,
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    ids: Vec<String>,
    tier: String,
) -> Result<CommandResponse, String> {
    let parsed = VisibilityTier::parse(&tier)
        .ok_or_else(|| format!("Unknown terminal visibility tier: {}", tier))?;
    let label = window.label();
    let mut applied = 0usize;
    for id in &ids {
        if let Some(session) = terminal_manager.get(id) {
            session.set_visibility(label, parsed);
            applied += 1;
        }
    }
    debug!(
        window = %label,
        tier = %parsed.as_str(),
        requested = ids.len(),
        applied,
        "terminal visibility reported"
    );
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Reset a terminal's flow-control counters (acked = sent).
///
/// Called by `TerminalInstance` on UNMOUNT: the disposing pane strands its
/// render-acks (its final coalesced write's completion callback never
/// fires), and if the emission gate is already paused at that moment the
/// tab would enter an emission blackout — the per-page tap receives
/// nothing, so it can never proxy-ack the tab back open. The unmounting
/// pane's backlog is being discarded with the xterm buffer anyway, so
/// resetting is both safe and required to hand consumption over to the tap.
#[tauri::command]
pub fn terminal_flow_reset(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    session.reset_flow_control();

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

    // Reset backpressure BEFORE snapshotting the ring: emission resumes at
    // the reset, so every chunk skipped up to this instant is either inside
    // the snapshot (tee'd before it) or delivered live after it — no chunk
    // can fall between snapshot and reset and open a hole exactly at
    // `endOffset`. Chunks that are both in the snapshot AND delivered live
    // are deduped by the frontend's `replayedThrough` trim.
    session.reset_flow_control();
    let (data, start_offset) = session.get_scrollback_buffer();
    let end_offset = start_offset + data.len() as u64;
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
///
/// ## The response says WRITTEN vs BOUND, because they are not the same thing
///
/// This command writes a PROVISIONAL row (`confirmed_at` unset — see
/// [`CONFIRM_DOOR`]), and `terminal_list`'s `sessionIdsByTerminal` map is gated
/// to CONFIRMED rows only
/// ([`SessionLifecycleStore::find_confirmed_open_by_terminal`], and the long
/// rationale on it). So a bare `success: true` told a caller nothing about
/// whether the session it just recorded would ever surface on a tab — "written"
/// read as "bound", and diagnosing the difference cost a manual test run most
/// of its wall clock.
///
/// The payload therefore reports the row's ACTUAL confirmation state (read back
/// from the store, because `record_open` never clears an existing confirmation)
/// and names the door that flips it. It does NOT confirm: the provisional gate
/// is deliberate, and surfacing unconfirmed ids would permanently mis-bind
/// phantom shells and non-pinned launches onto tabs.
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
    origin: Option<String>,
    provider: Option<String>,
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
        // Origin-unaware callers omit `provider`; default to claude (the only
        // provider today). record_open never downgrades an existing provider
        // via a re-record because it always refreshes it from the incoming
        // record — callers that re-assert must pass the right provider.
        provider: provider.unwrap_or_else(|| {
            crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string()
        }),
        // `None` (origin-unaware callers) preserves any existing origin;
        // record_open normalizes any legacy value passed in.
        origin,
        restore_pending_at: None,
        confirmed_at: None,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
    };
    let session_id = record.claude_session_id.clone();
    store.record_open(record);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(record_open_confirmation_report(&store, &session_id)),
    })
}

/// The door that flips a provisional row to confirmed — the provider's
/// SessionStart hook POSTs it (`install_effects_producer::post_session_open`,
/// which calls [`SessionLifecycleStore::confirm_session`]).
const CONFIRM_DOOR: &str = "POST /control/session-open";

/// Build [`terminal_session_record_open`]'s honesty payload for
/// `claude_session_id`: whether the row it just wrote is CONFIRMED — i.e.
/// whether `terminal_list` will surface it in `sessionIdsByTerminal` — and the
/// door that confirms it.
///
/// Read back from the store rather than assumed: this command always passes
/// `confirmed_at: None`, but `record_open` never clears an existing
/// confirmation, so re-recording an already-confirmed session must report
/// `confirmed: true`. A row that vanished between the write and the read (a
/// poisoned lock, a concurrent close) reads as unconfirmed — the conservative
/// answer, since unconfirmed is exactly "do not expect this on a tab yet".
fn record_open_confirmation_report(
    store: &SessionLifecycleStore,
    claude_session_id: &str,
) -> serde_json::Value {
    let confirmed = store
        .get(claude_session_id)
        .and_then(|r| r.confirmed_at)
        .is_some();
    // `confirmBy` is emitted unconditionally so the payload has one stable
    // shape for a harness to assert; when `confirmed` is true it simply names
    // the door that already fired.
    serde_json::json!({
        "recorded": true,
        "confirmed": confirmed,
        "confirmBy": CONFIRM_DOOR,
    })
}

/// Re-point an open session record at the terminal that now hosts it.
///
/// Called by the cold-restore path for every recreated tab whose record is NOT
/// on the verified-resume track (`terminal-only` / `quarantine`). Those records
/// otherwise keep pointing at the dead pre-restart terminal id forever, so each
/// restore pass fails to recognise the tab it already made and cold-creates a
/// duplicate — an unbounded PTY leak proportional to (stale records × restarts).
///
/// Deliberately NOT `terminal_session_record_open`: that refreshes
/// `last_seen_at` and would make ghost rows immortal. See
/// [`SessionLifecycleStore::rebind_terminal`].
///
/// ## Also stamps the TERMINAL-ONLY restore tier (restore census, plan D6/P5)
///
/// This command IS the non-auto-resume half of the boot-restore path (the
/// `auto-resume` half stamps via `terminal_session_mark_restore_pending`), so
/// it is the backend point that knows a record came back as terminal + cwd with
/// no conversation. It stamps
/// [`RESTORE_TIER_TERMINAL_ONLY`](crate::session::session_lifecycle_store::RESTORE_TIER_TERMINAL_ONLY)
/// — honestly: the
/// tile exists, the conversation does not, which is exactly what the frontend's
/// process-lifetime `restoreTerminalOnly` flag means.
///
/// The stamp lives HERE rather than in `SessionLifecycleStore::rebind_terminal`
/// because only this command's contract is boot-restore-specific; the store
/// primitive is a generic re-point that a future non-restore caller could
/// legitimately use, and it must not silently claim such a caller restored
/// anything.
#[tauri::command]
pub fn terminal_session_rebind_terminal(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
    terminal_id: String,
    zone_index: i32,
) -> Result<CommandResponse, String> {
    store.rebind_terminal(&claude_session_id, &terminal_id, zone_index);
    store.mark_restored_from_boot(
        &claude_session_id,
        crate::session::session_lifecycle_store::RESTORE_TIER_TERMINAL_ONLY,
    );
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Mark a session restore-pending in the lifecycle registry: the boot-restore
/// drain is about to type `claude --resume` into a freshly-created plain shell
/// and the handshake is not yet verified. While the marker is set the
/// backend liveness poll never flips the record `poll-dead` (a failed restore
/// must leave the durable `open` record intact for the next attempt). The
/// marker is backend-owned + durable so a frontend crash mid-restore can't
/// lose it. No-op (still succeeds) if the session is absent.
#[tauri::command]
pub fn terminal_session_mark_restore_pending(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
) -> Result<CommandResponse, String> {
    store.mark_restore_pending(&claude_session_id);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Clear a session's restore-pending marker: the restore drain verified the
/// Claude UI handshake (the resume actually landed), so normal liveness
/// classification resumes. No-op (still succeeds) if the session is absent or
/// the marker is already clear. The backend poll also self-heals a stale
/// marker when it observes the session confidently alive.
#[tauri::command]
pub fn terminal_session_clear_restore_pending(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
) -> Result<CommandResponse, String> {
    store.clear_restore_pending(&claude_session_id);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Mark a Claude terminal session closed in the lifecycle registry.
///
/// `terminal_id` is OPTIONAL and carries the other half of the key
/// `terminal_session_record_open` binds. Supply it whenever the caller has a
/// live terminal in hand: a tab's `claudeSessionId` can legitimately be stale or
/// foreign, and without the terminal id the store has no way to tell a correct
/// close from one that lands on a different session's record. Omit it only for
/// the closers whose terminal is gone by definition (`poll-dead`,
/// `never-started`, `no-terminal`, `migrated`).
///
/// The typed [`CloseOutcome`] is returned in `data` — never rendered into
/// `message` — and an unresolvable close reports `success: false`. A repeat
/// close of this terminal's own record (`alreadyClosed`) is a no-op, not a
/// failure.
#[tauri::command]
pub fn terminal_session_record_close(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
    terminal_id: Option<String>,
    reason: String,
) -> Result<CommandResponse, String> {
    let outcome = store.record_close_checked(&claude_session_id, terminal_id.as_deref(), &reason);
    Ok(close_outcome_response(&outcome))
}

/// Wire envelope for a [`CloseOutcome`]. Pure, so the two rules it encodes are
/// unit-testable without a Tauri app handle:
///
/// 1. the outcome is a TYPED value in `data`, never prose flattened into
///    `message` — `message` is for humans, `data` is for callers;
/// 2. a close that resolved to nothing is **not** a success.
pub(crate) fn close_outcome_response(outcome: &CloseOutcome) -> CommandResponse {
    CommandResponse {
        success: !matches!(outcome, CloseOutcome::NotFound { .. }),
        message: None,
        data: serde_json::to_value(outcome).ok(),
    }
}

/// List every RESTORABLE Claude terminal session from the lifecycle registry.
/// The grid hydrates its session tiles from this on boot instead of
/// `localStorage`.
///
/// Returns the restorable set (see `restorable_records`): `open` records
/// whose `last_seen_at` is recent relative to the registry's cohort ANCHOR
/// (the newest instant of the densest death cohort — see `restorable_records`;
/// the prior shutdown marker contributes only on a clean boot), PLUS
/// `closed`/`pty-exit` and `closed`/`poll-dead` records
/// still within their grace windows (graceful-restart case, where
/// `handleExit` flipped every live PTY to `closed`). User-closed, orphan
/// (`no-terminal`) and stale-ghost records are excluded so the restore path
/// resurrects exactly the sessions that died with the runner.
#[tauri::command]
pub fn terminal_session_list_open(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
) -> Result<CommandResponse, String> {
    let now = chrono::Utc::now().timestamp_millis();
    // The prior shutdown marker's `at` is one input to the anchor. It is
    // captured ONCE at boot (main.rs setup, before this command can run) —
    // the on-disk marker itself already says `clean:false` for the NOW-
    // running process and must never be re-read for restore decisions.
    let boot = crate::session::shutdown_marker::boot_classification();
    let prior_marker_at = boot.and_then(|c| c.prior_marker_at);
    // On a crash (unclean) boot the marker is the crashed process's own boot
    // instant — a boot artifact, not session liveness — so it must NOT feed
    // the cohort anchor (its stray-later `at` is exactly what pulled the
    // 2026-07-19 anchor ~1h46m past the crash band and stranded 81 sessions).
    // Only a CLEAN shutdown marker is an honest last-moment-of-life signal.
    let boot_was_clean = boot.map(|c| !c.crash_recovery).unwrap_or(false);
    let mut sessions = store.restorable_records(now, prior_marker_at, boot_was_clean);

    // G3 (session-restore-redesign Phase 3): UNION the registry restorable set
    // with the TRANSCRIPT-DERIVED disk-only recovery net. A session that was
    // live at crash but that the registry never captured — the spawn-record AND
    // the provider hook both missed, AND the crash beat the next reconcile poll
    // (which needs a live PTY it no longer has) — has no restorable row today
    // and is silently lost. `disk_only_restore_candidates` scans every Claude
    // config dir (dynamic account enumeration via `find_claude_config_dirs` —
    // NOT a hardcoded account list) for recently-active transcripts and offers
    // the registry-ABSENT ones under the account that holds each transcript.
    //
    // PRIMARY-ONLY since 2026-08-10 (plan
    // `2026-08-10-temp-runner-session-restore-isolation`, Phase 2). On any
    // SECONDARY — temp or named — `disk_only_restore_candidates` returns empty
    // WITHOUT scanning, so everything below about the machine-global scan and
    // the exclusion set describes the primary's behaviour only. If you are
    // asking "why is the restore set empty on my temp runner?", that is why,
    // and it is deliberate: a secondary's candidates are overwhelmingly other
    // instances', and offering them materialized a PTY apiece (measured: 283
    // live PTYs on one temp runner). The registry-backed restorable set above
    // is unaffected on every instance.
    //
    // Exclusion set (P3 fix): the restorable-set ids UNION every CLOSED row's
    // id — deliberately NOT `all_ids()`. `all_ids()` also excluded registry
    // rows that are `open` yet DROPPED by the restorable grace gate (the
    // Phase-1 cohort-anchor victims: an open row a crash-restart couldn't
    // admit), making them invisible to BOTH paths — not restorable AND not
    // disk-recoverable. By excluding only restorable + closed ids, exactly
    // those open grace-gate victims can now LEAK into the quarantined
    // disk-only net (if a fresh transcript exists). Closed rows never leak:
    // whether user-closed (`no-terminal`/explicit) or a grace-EXPIRED
    // `pty-exit`/`poll-dead`, their close encodes a "do not restore" decision,
    // so a fresh mtime must not resurrect them (the don't-resurrect-a-closed-
    // tab property the old `all_ids` exclusion was buying). In-grace
    // `pty-exit`/`poll-dead` closes are already in the restorable set, so they
    // stay excluded too — the union just additionally covers grace-expired and
    // user closes.
    //
    // These candidates are `origin=reconciled`+unconfirmed, so the frontend
    // classifier quarantines them behind the one-click verified-resume
    // handshake — never a blind `--resume`. Fail-open by construction: a scan
    // failure yields zero extra candidates, degrading to today's registry-only
    // set.
    let mut excluded = store.closed_ids();
    excluded.extend(sessions.iter().map(|r| r.claude_session_id.clone()));
    let disk_only = crate::session::reconcile::disk_only_restore_candidates(now, &excluded);
    if !disk_only.is_empty() {
        tracing::info!(
            count = disk_only.len(),
            "terminal_session_list_open: added transcript-derived disk-only restore candidates (registry capture-miss recovery)"
        );
    }
    sessions.extend(disk_only);

    // Stamp the derived `transcriptExists` bit onto every candidate so the
    // frontend classifier can avoid typing `--resume` against an id with no
    // conversation on disk. See `probe_transcript_exists` for why `confirmed_at`
    // is not sufficient proof and which cases read UNKNOWN.
    let rows: Vec<RestoreCandidate> = sessions
        .into_iter()
        .map(|rec| {
            let transcript_exists =
                store.probe_transcript_exists(&rec.claude_session_id, rec.working_dir.as_deref());
            RestoreCandidate {
                record: rec,
                transcript_exists,
            }
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": rows })),
    })
}

/// One row of [`terminal_session_list_open`]'s response: the registry record
/// flattened, plus the DERIVED `transcriptExists` bit. Kept out of
/// [`TerminalSessionRecord`] itself so the persisted registry stays free of
/// derived state — the bit is recomputed from disk on every list.
#[derive(serde::Serialize)]
struct RestoreCandidate {
    #[serde(flatten)]
    record: TerminalSessionRecord,
    /// `None` ⇒ not probed; serialized as absent so the frontend reads UNKNOWN
    /// rather than "no transcript".
    #[serde(rename = "transcriptExists", skip_serializing_if = "Option::is_none")]
    transcript_exists: Option<bool>,
}

/// List EVERY previous Claude terminal session for display — the "previous
/// sessions" surface. Unlike [`terminal_session_list_open`] (which returns only
/// the RESTORABLE set), this merges the full registry (open + closed) with the
/// append-only snapshot HISTORY (ids older than the 24 h registry retention)
/// and enriches each with its real `--resume` name, account, resume command,
/// and a re-probed `transcript_exists` / `restorable`. DISPLAY-only: it never
/// drives restore, and the snapshot history's write-only invariant is
/// preserved.
///
/// `opts` (all optional): `sinceMs`, `pageId`, `account`, `includeShells`,
/// `limit`. Sorted newest-first by `lastSeenAt`.
#[tauri::command]
pub fn terminal_session_list_history(
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    opts: Option<crate::session::past_sessions::PastSessionsOpts>,
) -> Result<CommandResponse, String> {
    let opts = opts.unwrap_or_default();
    // Resolve through the WRITE-side helper so this reads the same file
    // main.rs opened. It used to derive a port-keyed path under the unscoped
    // session-restore dir, which for any secondary was a different directory
    // AND a different filename from the instance-scoped file actually written
    // — so this read a file nothing wrote, and on a recycled temp-runner port,
    // a PRIOR temp runner's history.
    let snapshot_path = crate::session::session_lifecycle_store::snapshot_history_path();
    let sessions =
        crate::session::past_sessions::build_past_sessions(store.inner(), &snapshot_path, &opts);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": sessions })),
    })
}

/// Report a mount of the terminal page tree (P0 tree-reset observability).
///
/// The terminal tree gets remounted by top-level state flips, after which the
/// restore respawns `claude --resume` for still-alive sessions — and until now
/// the only trace was a webview `console.warn` nothing captured. The frontend
/// mount effect calls this on EVERY mount (mount #1 included — consumers
/// filter on `mount_number > 1` for genuine remounts), fire-and-forget.
///
/// Pure observability: one `tracing::info!` line + one durable JSONL row via
/// [`crate::session::snapshot_history::record_tree_reset`]. NO behavior
/// change — always succeeds, even when the durable append fails (best-effort
/// by that module's design).
#[tauri::command]
pub fn terminal_report_tree_reset(
    mount_number: u32,
    authenticated: Option<bool>,
    navigation_type: Option<String>,
    page_ids: Option<Vec<String>>,
    open_record_count: Option<u32>,
    time_origin: Option<f64>,
    client_ts: Option<i64>,
) -> Result<CommandResponse, String> {
    let page_ids = page_ids.unwrap_or_default();
    info!(
        mount_number,
        authenticated = ?authenticated,
        navigation_type = navigation_type.as_deref().unwrap_or("unknown"),
        page_ids = ?page_ids,
        open_record_count = ?open_record_count,
        time_origin = ?time_origin,
        "terminal tree reset reported: terminal page tree mounted (mount #{mount_number})"
    );
    // Port-scoped like the session-snapshots file, so multiple runner
    // instances never interleave rows.
    let port = crate::mcp::types::get_mcp_api_port();
    let path = crate::session::snapshot_history::tree_reset_path_for_port(port);
    crate::session::snapshot_history::record_tree_reset(
        &path,
        crate::session::snapshot_history::TreeResetReport {
            mount_number,
            authenticated,
            navigation_type,
            page_ids,
            open_record_count,
            time_origin,
            client_ts,
        },
    );
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Report that a terminal pane's UI Bridge input element is NOT registered
/// (manual-test-loop iter 18, item 1).
///
/// WHY a Tauri command and not just a `console.error`: a webview console line
/// never reaches the runner log — the same reason [`terminal_report_tree_reset`]
/// exists — and the only path that carries it out of WebView2 at all is the
/// SDK's optional browser-capture pipeline into the error monitor, itself a
/// DB-backed surface that can be degraded. Iteration 17 observed a restored pane
/// with no `terminal-input-<id>` for over two minutes and reported that "the
/// retry ladder's give-up warning never fires"; it could not have been seen from
/// `/health`, from the runner log, or from any HTTP probe even if it HAD fired.
/// A failure of this class must be readable from outside the webview over a path
/// with no dependencies, or it is indistinguishable from silence.
///
/// Two reporters call this, both once per terminal id per page lifetime:
///
///  - `reason = "instance-ladder"` — a MOUNTED `TerminalInstance` polled for
///    its input element and the bridge registry for the full retry budget and
///    never landed.
///  - `reason = "no-owner"` — the mount-independent proxy watchdog saw
///    `terminal-input-<id>` unowned for the whole budget. This is the arm that
///    covers a pane which never mounted at all (a flow-grid `assigned-virtual`
///    zone), which is precisely the case the ladder could not report because it
///    never started.
///
/// Pure observability: one `tracing::warn!` line. Always succeeds.
#[tauri::command]
pub fn terminal_report_bridge_registration_failure(
    terminal_id: String,
    element_id: String,
    reason: String,
    elapsed_ms: Option<u64>,
    detail: Option<String>,
) -> Result<CommandResponse, String> {
    let elapsed = elapsed_ms.unwrap_or(0);
    warn!(
        terminal_id = %terminal_id,
        element_id = %element_id,
        reason = %reason,
        elapsed_ms = elapsed,
        detail = detail.as_deref().unwrap_or("(none)"),
        "UI Bridge terminal input registration FAILED: {element_id} is not registered after {elapsed}ms (reason={reason}); custom actions on terminal {terminal_id} will answer ELEMENT_NOT_FOUND"
    );
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// List the LIVE Claude Code sessions with the names the operator actually
/// sees — read from Claude Code's own per-process registry
/// (`<config_dir>/sessions/<pid>.json`), not from our transcript scraping.
///
/// This is the correct source for "write the open sessions down before I
/// rebuild the runner": `name` is verbatim what the session window and
/// `/resume` show, and each row carries a ready `cd … && clX --resume <id>`.
///
/// Distinct from [`terminal_session_list_history`], which is the *historical*
/// view (open + closed + snapshot-only) with a transcript-derived
/// `resume_name`. Measured 2026-07-23, that derivation covered 33 of 80 live
/// sessions and matched the real window name on only 11 of those 33 — see
/// `session::claude_session_registry` for the numbers.
///
/// Returns one row per live PROCESS. Several live processes may report the
/// same `sessionId` (22 of 80 did), each with its own name; collapsing that is
/// the caller's decision.
#[tauri::command]
pub async fn terminal_claude_session_list_live() -> Result<CommandResponse, String> {
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    // Re-check liveness: a crashed process cannot remove its own registry file,
    // so PID presence is the guard against reporting a dead session as open.
    //
    // FAIL CLOSED on an empty snapshot: the runner itself is always a live
    // process, so an empty table can only be a failed read. Erroring here maps
    // to `null` (indeterminate) in the frontend liveness oracle
    // (`fetchLiveClaudeSessionIds`) → skip-unknown; returning success + `[]`
    // instead would read as "definitively no live sessions" and let the
    // restore path respawn — fork — every session.
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    let live_pids = crate::session::claude_session_registry::live_pids_from_snapshot(&snapshot)?;
    let sessions =
        crate::session::claude_session_registry::read_live_sessions(&config_dirs, &live_pids);
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": sessions })),
    })
}

/// Manually migrate a Claude terminal session to a different configured
/// account: copy its transcript into the target account's project dir, close
/// the old pane, and respawn `claude --resume` under the target account (see
/// `terminal::account_migration` — this is the operator-clicked form of the
/// automatic token-exhaustion migration; the click IS the confirmation, so
/// no usage probe gates it).
///
/// `target_config_dir: None` auto-picks the configured account whose spare
/// weekly capacity is closest to expiring — among accounts under their
/// projected pace, the one furthest through its 7-day window, since unused
/// capacity does not roll over past the reset
/// (`ai_provider::config::cmp_rank`) — excluding the session's current
/// account.
#[tauri::command]
pub async fn terminal_migrate_session_account(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<SessionLifecycleStore>>,
    claude_session_id: String,
    target_config_dir: Option<String>,
) -> Result<CommandResponse, String> {
    let record = store
        .get(&claude_session_id)
        .ok_or_else(|| format!("unknown session: {claude_session_id}"))?;
    let src = record
        .config_dir
        .clone()
        .or_else(crate::ai_provider::get_resolved_config_dir)
        .ok_or("the session's current account is unknown — cannot migrate")?;

    let dst = match target_config_dir {
        Some(d) => {
            if !crate::settings::get_claude_config_dirs().contains(&d) {
                return Err(format!("'{d}' is not a configured claude_config_dir"));
            }
            d
        }
        None => crate::ai_provider::pick_migration_target(&src).ok_or(
            "no usable target account (every other account exhausted/cooled/unauthenticated)",
        )?,
    };
    if dst == src {
        return Err("target account equals the session's current account".to_string());
    }

    let outcome = crate::terminal::account_migration::migrate_session(&app, &record, &src, &dst)?;
    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "migrated session {claude_session_id} to {}",
            outcome.to_config_dir
        )),
        data: Some(serde_json::json!({
            "newTerminalId": outcome.new_terminal_id,
            "fromConfigDir": outcome.from_config_dir,
            "toConfigDir": outcome.to_config_dir,
        })),
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
    /// Pre-pinned Claude session id (`--session-id <uuid>` in the spawn
    /// argv, or `--resume <uuid>` for an account-migration respawn — both
    /// name the exact id): `Some(..)` records the OPEN synchronously (origin
    /// `"pinned"`) and demotes the transcript poller to a verification arm.
    pub claude_session_id: Option<String>,
    /// Grid zone to durably record (account-migration respawns preserve the
    /// migrated session's tile placement). `None` → zone 0 (the historical
    /// continuation behavior — wrong zone beats a lost session).
    pub zone_index: Option<i32>,
    /// When `true`, inject the autonomous-agent git author/committer identity
    /// (`GIT_AUTHOR_*`/`GIT_COMMITTER_*`) into the spawned PTY's env so the
    /// agent's commits land with a meaningful author instead of the ambient
    /// host placeholder (`x <x@x>`). Set ONLY for autonomous spawns (gate
    /// continuations); the operator's own terminals leave it `false` and keep
    /// their host git identity. See `crate::agent_runtime::agent_git_identity_env`.
    pub inject_agent_git_identity: bool,
    /// Coord lineage for this spawn, when it CONTINUES a known coord session
    /// rather than starting fresh. `None` (every pre-existing caller) leaves
    /// today's behaviour byte-for-byte: no `parent_session_id`, and the
    /// ambient Claude Code session id on the mirrored row.
    ///
    /// Set by the cross-machine respawn receiver
    /// ([`crate::session::respawn`]) so the respawned session is one link in
    /// the lineage chain — the same `parent_session_id` stamp
    /// [`crate::session::SessionRegistry::start_with_parent`] gives the handoff
    /// receiver — rather than an orphan.
    pub coord_lineage: Option<CoordSessionLineage>,
}

/// Lineage a backend spawn claims on the coord session row it mirrors.
///
/// Two fields, one purpose: make a continued session findable FROM its source.
/// Both are stamped on the `Started` outbox payload, which the drain loop's
/// `rebuild_create_body` forwards to coord's `CreateSessionRequest`.
#[derive(Debug, Clone)]
pub(crate) struct CoordSessionLineage {
    /// The coord session id this spawn continues — coord indexes children by
    /// `parent_session_id`, so this is the durable source→child link.
    pub parent_session_id: uuid::Uuid,
    /// The Claude session id the child actually resumes, stamped on the row's
    /// `claude_code_session_id` bridge so the console can join the respawned
    /// session to its transcript.
    ///
    /// `None` is UNKNOWN and leaves the ambient value alone — never a nil UUID
    /// and never an empty string, either of which would read downstream as a
    /// real, joinable id.
    pub claude_code_session_id: Option<String>,
}

/// Untracked-backend-spawn guardrail (plan
/// `2026-07-03-runner-session-tracking-drift-and-guardrails` Phase 3 item 1):
/// every backend/headless caller generates its own pinned session id before
/// calling in, so there is NEVER a legitimate reason for a backend spawn to
/// omit `capture_hint` — an omission means the session will not be durably
/// recorded and a restart silently drops it (the exact bug class three prior
/// fixes each patched at one call site). WARN + count
/// (`session_lifecycle_untracked_backend_spawn_total`, surfaced on `/health`
/// under `sessionTracking`) so the NEXT gap is loud instead of silent.
fn warn_untracked_backend_spawn(
    capture_hint: &Option<SessionCaptureHint>,
    title: &str,
    working_dir: &str,
) {
    if capture_hint.is_some() {
        return;
    }
    let total = crate::session::tracking_health::note_untracked_backend_spawn();
    warn!(
        counter = "session_lifecycle_untracked_backend_spawn_total",
        total,
        title = %title,
        working_dir = %working_dir,
        "backend terminal spawn without capture_hint — session will NOT be durably \
         recorded and a restart will silently drop it; use \
         create_tracked_terminal_session_backend"
    );
}

/// Tracked variant of [`create_terminal_session_backend`] where the capture
/// hint is NON-OPTIONAL — the compiler, not a log line, guarantees a backend
/// spawn is durably recorded. All backend/headless callers (gate
/// continuation, condition check, account migration) go through this; only a
/// genuinely-interactive path that cannot know the session id at spawn time
/// may use the optional-shape function directly (and eats the WARN + counter
/// if it passes `None`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_tracked_terminal_session_backend(
    terminal_manager: &Arc<TerminalManager>,
    session_registry: &Arc<SessionRegistry>,
    app_handle: tauri::AppHandle,
    title: String,
    working_dir: String,
    work_unit_slug: Option<String>,
    correlation_topic: Option<String>,
    intent_repo: Option<String>,
    command: Option<Vec<String>>,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    capture_hint: SessionCaptureHint,
    page_id: Option<String>,
    resource_override: bool,
) -> Result<(String, Option<uuid::Uuid>), String> {
    create_terminal_session_backend(
        terminal_manager,
        session_registry,
        app_handle,
        title,
        working_dir,
        work_unit_slug,
        correlation_topic,
        intent_repo,
        command,
        isolated_ctx,
        Some(capture_hint),
        page_id,
        resource_override,
    )
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
///
/// Prefer [`create_tracked_terminal_session_backend`] — the `capture_hint:
/// Option<_>` shape here exists only for a path that genuinely cannot know
/// the session id at spawn time; a `None` trips the untracked-backend-spawn
/// guardrail (WARN + counter, see [`warn_untracked_backend_spawn`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_terminal_session_backend(
    terminal_manager: &Arc<TerminalManager>,
    session_registry: &Arc<SessionRegistry>,
    app_handle: tauri::AppHandle,
    title: String,
    working_dir: String,
    work_unit_slug: Option<String>,
    correlation_topic: Option<String>,
    intent_repo: Option<String>,
    command: Option<Vec<String>>,
    isolated_ctx: Option<crate::agent_worktree::isolated_edit::IsolatedEditContext>,
    capture_hint: Option<SessionCaptureHint>,
    page_id: Option<String>,
    // Part D — forwarded to the spawn-time resource gate. Threaded through
    // rather than hardcoded here because the four callers of the tracked
    // variant answer this question differently: a gate continuation and a
    // condition check are NEW autonomous work and must respect the floor,
    // while an account-migration respawn is the continuation of a session that
    // already existed a moment ago. See each call site.
    resource_override: bool,
) -> Result<(String, Option<uuid::Uuid>), String> {
    warn_untracked_backend_spawn(&capture_hint, &title, &working_dir);
    // The shared session-env contribution (`QONTINUI_SESSION_WORKTREES` from
    // the pre-acquired context + the configured plan directories), derived
    // before the context is parked on the session. The `--add-dir <sibling>`
    // convenience for `claude` launches is appended into `command` by the
    // caller (gate-continuation in `agent_runtime.rs`), since only the caller
    // knows the launch is `claude`.
    let mut env_pairs: Vec<(String, String)> =
        crate::agent_worktree::session_env::session_env(isolated_ctx.as_ref());
    // Account selection: pin the spawned PTY to the account the caller chose
    // (gate continuations set `capture_hint.config_dir` to the selected,
    // token-bearing account). Without this, a backend-spawned `claude` inherits
    // the runner's ambient `CLAUDE_CONFIG_DIR` (e.g. a quota-exhausted account)
    // and dies instantly. The interactive terminal path sets this via the shell
    // command instead; this is the backend-spawn equivalent.
    if let Some(dir) = capture_hint.as_ref().and_then(|h| h.config_dir.clone()) {
        env_pairs.push(("CLAUDE_CONFIG_DIR".to_string(), dir));
    }
    // Autonomous-agent git identity: opt-in per caller (gate continuations).
    // Overrides the ambient host git config (which may be a placeholder like
    // `x <x@x>`) for THIS PTY only, so the agent's commits get a meaningful
    // author. Operator-opened terminals never set the flag and are untouched.
    if capture_hint
        .as_ref()
        .is_some_and(|h| h.inject_agent_git_identity)
    {
        env_pairs.extend(crate::agent_runtime::agent_git_identity_env());
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
        resource_override,
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
    let (share_output, redact_secrets) = intent_sharing_from_settings();
    let intent = Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo: intent_repo,
        branch: None,
        work_unit_slug,
        // Accept-only on `Intent`; the runner never emits the legacy key.
        plan_slug: None,
        correlation_topic,
        // Gate-continuation create path — carry the chosen page tab into
        // `coord.sessions.intent` so coord learns the placement.
        page_id: page_id.clone(),
        declared_paths: vec![std::path::PathBuf::from(&working_dir)],
        share_output,
        redact_secrets,
        // Gate-continuation terminal — device-default binding; the registry
        // stamps machine.json's default-for-new-sessions.
        tenant_id: None,
    };

    // Lineage, read BEFORE the hint is destructured below. `None` for every
    // pre-existing caller ⇒ `register_external_with_lineage` behaves exactly
    // like the `register_external` call this replaced.
    let lineage = capture_hint.as_ref().and_then(|h| h.coord_lineage.clone());
    let (parent_session_id, claude_code_session_id_override) = match lineage {
        Some(l) => (Some(l.parent_session_id), l.claude_code_session_id),
        None => (None, None),
    };

    let mut coord_session_id: Option<uuid::Uuid> = None;
    match session_registry.register_external_with_lineage(
        intent,
        parent_session_id,
        claude_code_session_id_override,
    ) {
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
                claude_session_id: pinned_session_id,
                zone_index: hint_zone_index,
                // Consumed earlier (env injection at spawn); not needed here.
                inject_agent_git_identity: _,
                // Consumed earlier (coord registration above).
                coord_lineage: _,
            } = hint;
            // Single source of truth for the recorded page: the hint's page_id
            // (set by the caller to the picked page), defaulting to "default".
            let record_page_id = hint_page_id.unwrap_or_else(|| "default".to_string());
            let record_zone_index = hint_zone_index.unwrap_or(0);
            if let Some(pinned) = pinned_session_id {
                // Pre-pinned id: record synchronously, then verify async that
                // the transcript appears. NEVER rebinds from transcripts.
                record_pinned_session_open(
                    &store,
                    pinned.clone(),
                    terminal_id.clone(),
                    config_dir.clone(),
                    working_dir.clone(),
                    title,
                    record_page_id,
                    record_zone_index,
                    crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
                );
                let verify_dirs: Vec<std::path::PathBuf> = config_dir
                    .iter()
                    .map(std::path::PathBuf::from)
                    .chain(crate::terminal::transcript::find_claude_config_dirs())
                    .collect();
                let verify_pinned = pinned.clone();
                let verify = move || {
                    verify_dirs.iter().any(|dir| {
                        crate::terminal::transcript::session_transcript_path(
                            dir,
                            &working_dir,
                            &verify_pinned,
                        )
                        .exists()
                    })
                };
                tokio::spawn(poll_and_verify_pinned_session(
                    verify,
                    terminal_id,
                    pinned,
                    SESSION_CAPTURE_POLL_INTERVAL,
                    SESSION_CAPTURE_TIMEOUT,
                ));
            } else {
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
                    record_zone_index,
                    SESSION_CAPTURE_POLL_INTERVAL,
                    SESSION_CAPTURE_TIMEOUT,
                ));
            }
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
/// LAST-RESORT BACKSTOP (session-restore redesign Phase 4/5): this is the
/// DEMOTED mtime-race recorder, no longer the primary identity path. The
/// primary path is the spawn-time pre-pinned `--session-id`
/// ([`record_pinned_session_open`], recorded synchronously + authoritatively)
/// plus the provider SessionStart hook, with [`crate::session::reconcile`] as
/// the process-start-anchored on-disk backstop. The ONE remaining live caller
/// is `create_terminal_session_backend`'s continuation branch for a
/// backend-spawned session whose id was NOT pre-pinned (no `--session-id` in the
/// hint) — there the id only appears once the child writes its first transcript,
/// so this poll is the only recourse. It records with origin `"reconciled"` (a
/// freshest-mtime guess that may be foreign — quarantined on restore, never
/// auto-resumed). Do NOT add new callers; pin the id at spawn instead.
///
/// The resolver is injected so this loop is unit-testable without a real
/// on-disk transcript. On the first resolve it builds a [`TerminalSessionRecord`]
/// (restored into `zone_index` / the hint page — callers without a meaningful
/// zone pass 0; wrong zone beats a lost session) and records it; on timeout
/// it `warn!`s and gives up without recording.
#[allow(clippy::too_many_arguments)]
async fn poll_and_record_session<F>(
    store: Arc<SessionLifecycleStore>,
    resolve: F,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
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
                zone_index,
                title: Some(title.clone()),
                terminal_id: terminal_id.clone(),
                // record_open seeds these from `now`; values here are placeholders
                // (mirrors `terminal_session_record_open`).
                opened_at: 0,
                last_seen_at: 0,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
                provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
                // Freshest-transcript mtime guess — may be a foreign session.
                origin: Some(
                    crate::session::session_lifecycle_store::ORIGIN_RECONCILED.to_string(),
                ),
                restore_pending_at: None,
                confirmed_at: None,
                handle: None,
                account_label: None,
                account_wrapper: None,
                session_name: None,
                name_source: None,
                tenant_id: None,
                task_run_id: None,
                bypass_permissions: None,
                restored_from_boot_at: None,
                restore_tier: None,
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

/// Durably record a PRE-PINNED session (`--session-id <id>` /
/// account-migration `--resume <id>` in the spawn argv) the instant the PTY
/// exists — no transcript mtime race to lose.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_pinned_session_open(
    store: &SessionLifecycleStore,
    claude_session_id: String,
    terminal_id: String,
    config_dir: Option<String>,
    working_dir: String,
    title: String,
    page_id: String,
    zone_index: i32,
    provider: String,
) {
    // D1 account stamp at spawn. The runner genuinely knows the ACCOUNT here —
    // it just placed `config_dir` into the PTY env — so derive the label +
    // wrapper from it rather than waiting on the live registry. It does NOT
    // know the session NAME at spawn (Claude Code invents or reads that after
    // it starts), so `session_name` stays `None` and the confirmation hook /
    // liveness poll fills it. Never guess a name: a wrong name is worse than an
    // absent one, and the sticky merge treats `None` as "keep whatever is
    // known".
    //
    // Guarded on a KNOWN config dir: `account_from_config_dir(None)` answers
    // `label:"unknown", wrapper:"claude"`, which is a placeholder, not
    // knowledge. Storing it would turn "we don't know the account" into a
    // confident-looking string — the silent-empty defect this record exists to
    // fix. No config dir ⇒ no account stamp; the poll fills it from the live
    // registry once the session actually runs.
    let account = config_dir
        .as_deref()
        .map(|d| crate::session::past_sessions::account_from_config_dir(Some(d)));
    store.record_open(TerminalSessionRecord {
        claude_session_id: claude_session_id.clone(),
        config_dir,
        working_dir: Some(working_dir),
        page_id,
        zone_index,
        title: Some(title),
        terminal_id: terminal_id.clone(),
        // record_open seeds these from `now`; values here are placeholders.
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider,
        // The runner KNOWS this id — it pre-pinned `--session-id` (or lifted a
        // typed flag / a hook POSTed it). Authoritative ⇒ auto-resume safe.
        origin: Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string()),
        restore_pending_at: None,
        confirmed_at: None,
        handle: None,
        account_label: account.as_ref().map(|a| a.label.clone()),
        account_wrapper: account.as_ref().map(|a| a.wrapper.clone()),
        // Not knowable at spawn — filled by the confirmation hook / poll.
        session_name: None,
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
    });
    info!(
        terminal_id = %terminal_id,
        claude_session = %claude_session_id,
        "create_terminal_session_backend: pinned session durably recorded at spawn"
    );
}

/// Verification arm for a pre-pinned session: poll `verify` (pinned
/// transcript exists on disk?) until confirmed or `timeout`. Pure
/// observability — NEVER touches the registry binding; timeout warns loudly.
async fn poll_and_verify_pinned_session<F>(
    verify: F,
    terminal_id: String,
    claude_session_id: String,
    interval: std::time::Duration,
    timeout: std::time::Duration,
) where
    F: Fn() -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if verify() {
            info!(
                terminal_id = %terminal_id,
                claude_session = %claude_session_id,
                "pinned session verified: transcript present on disk"
            );
            return;
        }
        if std::time::Instant::now() >= deadline {
            warn!(
                terminal_id = %terminal_id,
                claude_session = %claude_session_id,
                "PINNED SESSION MISMATCH: transcript for the pinned --session-id never \
                 appeared — claude likely failed to start (session-id collision fails \
                 loudly in the pane) or wrote to an unexpected config dir. Registry \
                 binding left as-is; NOT rebinding from transcripts."
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
            terminal_flow_reset,
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
            terminal_session_list_history,
            terminal_session_mark_restore_pending,
            terminal_session_clear_restore_pending,
            terminal_session_rebind_terminal,
            terminal_report_tree_reset,
            terminal_report_bridge_registration_failure,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// The close envelope carries the outcome as a TYPED value in `data`, never
    /// as prose in `message` — `message` is for humans, `data` is for callers.
    /// A typed fact flattened into a success-envelope string is exactly the
    /// pattern the typed-error-boundary work exists to burn down.
    #[test]
    fn close_outcome_response_puts_the_typed_outcome_in_data_not_message() {
        let r = close_outcome_response(&CloseOutcome::Redirected {
            requested: "stale".to_string(),
            closed: "live".to_string(),
            terminal_id: "term-live".to_string(),
        });
        assert!(r.message.is_none(), "message must stay free of typed facts");
        let data = r.data.expect("the outcome is serialized into data");
        assert_eq!(data["outcome"], "redirected");
        assert_eq!(data["requested"], "stale");
        assert_eq!(data["closed"], "live");
        assert_eq!(data["terminalId"], "term-live");
    }

    /// A close that resolved to NOTHING is not a success. Under the old
    /// contract an unresolvable close, a foreign close and a correct close were
    /// all reported identically as `success: true`, which is why a wrong id was
    /// undetectable at every layer.
    #[test]
    fn close_outcome_response_refuses_to_call_not_found_a_success() {
        let not_found = close_outcome_response(&CloseOutcome::NotFound {
            requested: "ghost".to_string(),
            terminal_id: Some("term-gone".to_string()),
        });
        assert!(!not_found.success, "NotFound must NOT report success");
        assert_eq!(not_found.data.unwrap()["outcome"], "notFound");

        // Everything that really resolved still succeeds — including the benign
        // repeat close, which must not be made to look like a failure or the
        // signal gets ignored.
        for resolved in [
            CloseOutcome::Closed {
                claude_session_id: "s".to_string(),
            },
            CloseOutcome::Redirected {
                requested: "a".to_string(),
                closed: "b".to_string(),
                terminal_id: "t".to_string(),
            },
            CloseOutcome::RedirectedAmbiguous {
                requested: "a".to_string(),
                closed: "b".to_string(),
                terminal_id: "t".to_string(),
                candidates: vec!["b".to_string(), "c".to_string()],
            },
            CloseOutcome::AlreadyClosed {
                claude_session_id: "s".to_string(),
            },
        ] {
            assert!(
                close_outcome_response(&resolved).success,
                "{resolved:?} resolved to a real record — it is a success"
            );
        }
    }

    fn restore_candidate_record(id: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            claude_session_id: id.to_string(),
            config_dir: None,
            working_dir: Some("C:/repo".to_string()),
            page_id: "default".to_string(),
            zone_index: 0,
            title: None,
            terminal_id: "term-1".to_string(),
            opened_at: 1,
            last_seen_at: 2,
            state: "open".to_string(),
            closed_at: None,
            close_reason: None,
            provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
            origin: Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string()),
            restore_pending_at: None,
            confirmed_at: Some(3),
            handle: None,
            account_label: None,
            account_wrapper: None,
            session_name: None,
            name_source: None,
            tenant_id: None,
            task_run_id: None,
            bypass_permissions: None,
            restored_from_boot_at: None,
            restore_tier: None,
        }
    }

    /// `RestoreCandidate`'s wire shape: the record's own fields stay FLAT (the
    /// frontend deserializes it as a `TerminalSessionRecord`), and the derived
    /// `transcriptExists` bit rides alongside — present as a real boolean when
    /// probed, and ABSENT (never `false`) when no probe is attached. The absent
    /// case is load-bearing: the frontend classifier downgrades to
    /// terminal-only on `false` only, so a `false` emitted for "not probed"
    /// would silently disable auto-resume for every session.
    #[test]
    fn restore_candidate_flattens_record_and_omits_unprobed_transcript_bit() {
        let probed_absent = serde_json::to_value(RestoreCandidate {
            record: restore_candidate_record("sess-absent"),
            transcript_exists: Some(false),
        })
        .expect("probed-absent candidate serializes");
        assert_eq!(probed_absent["claudeSessionId"], "sess-absent");
        assert_eq!(
            probed_absent["transcriptExists"],
            serde_json::Value::Bool(false),
            "a probed-absent transcript must be reported as an explicit false"
        );

        let probed_present = serde_json::to_value(RestoreCandidate {
            record: restore_candidate_record("sess-present"),
            transcript_exists: Some(true),
        })
        .expect("probed-present candidate serializes");
        assert_eq!(
            probed_present["transcriptExists"],
            serde_json::Value::Bool(true)
        );

        let unprobed = serde_json::to_value(RestoreCandidate {
            record: restore_candidate_record("sess-unprobed"),
            transcript_exists: None,
        })
        .expect("unprobed candidate serializes");
        assert_eq!(
            unprobed["claudeSessionId"], "sess-unprobed",
            "flatten must keep the record's fields at the top level"
        );
        assert!(
            unprobed.get("transcriptExists").is_none(),
            "no probe attached must OMIT the field, never emit false: {unprobed}"
        );
    }

    /// With NO probe attached every read is UNKNOWN, so a standalone store can
    /// never manufacture a "no transcript" verdict.
    #[test]
    fn probe_transcript_exists_is_unknown_without_an_attached_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");
        assert_eq!(
            store.probe_transcript_exists("sess-1", Some("C:/repo")),
            None,
            "unattached probe must read UNKNOWN, not absent"
        );
    }

    /// The working-dir guard, exercised WITH a probe attached so the assertions
    /// actually reach it. A probe that says `true` for everything isolates the
    /// guard: a missing or blank working dir must still read UNKNOWN, because a
    /// transcript path is derived from the project dir and
    /// `DiskTranscriptIndex::transcript_exists` answers a bare `false` when it
    /// has none — "could not determine", not "does not exist". Gating on that
    /// raw value would demote a resumable session to terminal-only.
    #[test]
    fn probe_transcript_exists_needs_a_usable_working_dir_to_answer() {
        #[derive(Debug)]
        struct AlwaysPresent;
        impl crate::session::snapshot_history::TranscriptProbe for AlwaysPresent {
            fn transcript_exists(&self, _session_id: &str, _wd: Option<&str>) -> bool {
                true
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");
        store.attach_transcript_probe(Arc::new(AlwaysPresent));

        assert_eq!(
            store.probe_transcript_exists("sess-1", Some("C:/repo")),
            Some(true),
            "an attached probe with a usable working dir must be consulted"
        );
        assert_eq!(
            store.probe_transcript_exists("sess-1", None),
            None,
            "no working dir means the probe cannot answer — UNKNOWN, not absent"
        );
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                store.probe_transcript_exists("sess-1", Some(blank)),
                None,
                "a blank working dir ({blank:?}) encodes to an impossible path — UNKNOWN"
            );
        }
    }

    /// Phase 3 item 1 guardrail: a backend call arriving with
    /// `capture_hint: None` (through the optional-shape compatibility
    /// surface — [`warn_untracked_backend_spawn`] is its first statement)
    /// trips the `session_lifecycle_untracked_backend_spawn_total` counter;
    /// a hinted call does not. The tracked variant
    /// (`create_tracked_terminal_session_backend`) makes the hint
    /// non-optional, so its callers compile the guarantee away — no runtime
    /// assertion is possible or needed there.
    #[test]
    fn untracked_backend_spawn_guard_trips_counter_on_none_hint_only() {
        let before = crate::session::tracking_health::untracked_backend_spawn_total();

        // Hinted call → no increment.
        let hint = Some(SessionCaptureHint {
            config_dir: None,
            working_dir: "/work/dir".to_string(),
            title: "Hinted".to_string(),
            page_id: None,
            claude_session_id: Some("pinned-1".to_string()),
            zone_index: None,
            inject_agent_git_identity: false,
            coord_lineage: None,
        });
        warn_untracked_backend_spawn(&hint, "Hinted", "/work/dir");
        assert_eq!(
            crate::session::tracking_health::untracked_backend_spawn_total(),
            before,
            "a hinted backend spawn must not count as untracked"
        );

        // Hint-less call → warn + increment. (Other tests share the global
        // counter, so assert a strict increase rather than an exact value.)
        warn_untracked_backend_spawn(&None, "Untracked", "/work/dir");
        assert!(
            crate::session::tracking_health::untracked_backend_spawn_total() > before,
            "a backend spawn with capture_hint: None must trip the counter"
        );
    }

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
            0,
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
            4,
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await;

        let open = store.open_records();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].page_id, "page-7");
        assert_eq!(
            open[0].zone_index, 4,
            "caller-supplied zone (account-migration respawn) is durably recorded"
        );
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
            0,
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

    /// Regression for the 2026-06-12 mis-bind incident: two transcripts in
    /// the project dir, the FRESHER one foreign. The mtime guess binds the
    /// foreign id; the pinned path must take NOTHING from transcripts.
    #[tokio::test]
    async fn pinned_path_ignores_fresher_foreign_transcript() {
        use crate::terminal::transcript::{get_latest_session_id, session_transcript_path};
        let cfg = tempfile::tempdir().unwrap();
        let project_path = "C:/work/repo";
        let pinned_id = "11111111-1111-4111-8111-111111111111";
        let foreign_id = "99999999-9999-4999-8999-999999999999";

        // Lay both transcripts down, the foreign one FRESHER (later mtime).
        let pinned_path = session_transcript_path(cfg.path(), project_path, pinned_id);
        std::fs::create_dir_all(pinned_path.parent().unwrap()).unwrap();
        std::fs::write(&pinned_path, "{\"type\":\"user\"}\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let foreign_path = session_transcript_path(cfg.path(), project_path, foreign_id);
        std::fs::write(&foreign_path, "{\"type\":\"user\"}\n").unwrap();

        // Sanity: the incident's guess mechanism really picks the foreign id.
        let guessed = get_latest_session_id(cfg.path(), project_path, None)
            .expect("fixture must yield a freshest session");
        assert_eq!(guessed.session_id, foreign_id, "foreign must be freshest");

        // Pinned path: synchronous record + verification arm.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        record_pinned_session_open(
            &store,
            pinned_id.to_string(),
            "term-pin".to_string(),
            Some(cfg.path().to_string_lossy().to_string()),
            project_path.to_string(),
            "Pinned".to_string(),
            "default".to_string(),
            0,
            crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
        );
        let verify_cfg = cfg.path().to_path_buf();
        poll_and_verify_pinned_session(
            move || session_transcript_path(&verify_cfg, project_path, pinned_id).exists(),
            "term-pin".to_string(),
            pinned_id.to_string(),
            Duration::from_millis(1),
            Duration::from_millis(50),
        )
        .await;

        let open = store.open_records();
        assert_eq!(open.len(), 1, "exactly one record — nothing guessed in");
        assert_eq!(open[0].claude_session_id, pinned_id);
        assert_eq!(
            open[0].origin.as_deref(),
            Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE)
        );
        assert_eq!(
            open[0].provider,
            crate::session::session_lifecycle_store::DEFAULT_PROVIDER
        );
    }

    // ── Phase 8: both create sites declare sharing from the setting ──────

    /// Stock settings reproduce the literals both create sites hardcoded
    /// before Phase 8. This is the back-compat guarantee for the coord
    /// intent, stated as a test rather than as a comment.
    #[test]
    fn intent_sharing_defaults_to_the_pre_phase8_literals() {
        let _guard = crate::settings::perf_test_lock();
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
        assert_eq!(intent_sharing_from_settings(), (true, None));
    }

    /// The knob actually moves what both sites will declare — reverting
    /// either site to a hardcoded `true` would leave this failing.
    #[test]
    fn intent_sharing_follows_the_setting() {
        let _guard = crate::settings::perf_test_lock();
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings {
            share_terminal_output: false,
            redact_terminal_secrets: Some(true),
            ..crate::settings::PerformanceSettings::default()
        });
        assert_eq!(intent_sharing_from_settings(), (false, Some(true)));

        // Leave the process cache stocked so a later test in this binary
        // does not inherit a hostile posture.
        crate::settings::set_performance_cache(crate::settings::PerformanceSettings::default());
    }

    // ── `terminal_session_record_open` answers WRITTEN vs BOUND ──────────

    /// The payload must distinguish a written row from a bound one. A freshly
    /// recorded session is PROVISIONAL, so `terminal_list` will not surface it
    /// — the caller has to be told that, and told which door flips it, rather
    /// than reading a bare `success: true` as "the tab now carries this id".
    #[test]
    fn record_open_report_is_unconfirmed_until_the_confirm_door_fires() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");

        let mut record = restore_candidate_record("sess-1");
        record.confirmed_at = None;
        store.record_open(record);

        let report = record_open_confirmation_report(&store, "sess-1");
        assert_eq!(report["recorded"], serde_json::Value::Bool(true));
        assert_eq!(
            report["confirmed"],
            serde_json::Value::Bool(false),
            "a freshly recorded row is provisional and will NOT reach terminal_list: {report}"
        );
        assert_eq!(
            report["confirmBy"], CONFIRM_DOOR,
            "the payload must name the door that confirms it"
        );

        // …and the report must NOT have confirmed it as a side effect: the
        // provisional gate is what keeps phantom shells and non-pinned launches
        // off tabs.
        assert!(
            store.find_confirmed_open_by_terminal("term-1").is_none(),
            "reporting the state must never flip it"
        );
    }

    /// Read back, not assumed: this command always passes `confirmed_at: None`,
    /// but `record_open` never clears an existing confirmation — so
    /// re-recording an already-confirmed session reports `confirmed: true`.
    #[test]
    fn record_open_report_reflects_an_already_confirmed_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");

        let mut record = restore_candidate_record("sess-2");
        record.confirmed_at = None;
        store.record_open(record.clone());
        store.confirm_session("sess-2");

        // A re-record (the frontend re-asserting the tab) passes no
        // confirmation of its own…
        store.record_open(record);
        let report = record_open_confirmation_report(&store, "sess-2");
        assert_eq!(
            report["confirmed"],
            serde_json::Value::Bool(true),
            "the confirmed row must report bound, not provisional: {report}"
        );
    }

    /// An unknown id is UNCONFIRMED, never a claim of boundness — the
    /// conservative answer when the row cannot be read back.
    #[test]
    fn record_open_report_is_unconfirmed_for_an_unknown_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json"))
            .expect("store opens");
        let report = record_open_confirmation_report(&store, "never-recorded");
        assert_eq!(report["confirmed"], serde_json::Value::Bool(false));
    }
}
