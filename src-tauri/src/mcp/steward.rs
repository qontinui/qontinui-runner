//! HTTP control surface for the runner's **steward** sessions.
//!
//! Modeled on `mcp/terminals.rs`'s `create_terminal_handler` /
//! `list_terminals_handler` (the closest template — NOT `mcp/worktrees.rs`,
//! which is cited only for its `ApiResponse` envelope shape): a titled PTY
//! terminal session is a steward's entire lifecycle. Opening the tab starts
//! it; closing the tab (killing the PTY child) stops it. There is at most one
//! session **per steward kind**, tracked by terminal **id** in
//! [`steward_meta_store`] (see [`find_running_steward`] for why title cannot
//! serve as the marker — discovered in manual testing, corrects the plan's
//! original §6 Q2 resolution).
//!
//! `POST /steward/{kind}/start` spawns the PTY **server-side** (mirroring
//! `create_terminal_handler`, `mcp/terminals.rs:137`) so the identical code
//! path serves both a UI button (which just calls this endpoint instead of
//! hand-rolling open-tab + type-command) and an agent driving it via `curl`
//! (which has no frontend to type into — server-side spawn is necessary for
//! parity, not just simpler; see plan §6 Q3).
//!
//! # Why this module is a roster and not three constants
//!
//! It launched exactly one steward (`merge-train-steward`) from three
//! module-level constants until 2026-08-28. Every other steward skill the
//! fleet has — `dev-ops-steward`, `cleanup-steward` — therefore had **no
//! supported launch path at all**, and in particular no path that sets the
//! enablement env var its own kill-switch reads. A `/dev-ops-steward` session
//! started by hand ran for 50 iterations with `COORD_DEVOPS_STEWARD_ENABLED`
//! unset: nothing on the machine wrote it, so the documented way to stop that
//! steward ("unset the flag") was inert, because the flag was never set in the
//! first place. Adding a steward is now one [`STEWARDS`] row.

use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Manager;
use tracing::{error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::types::TerminalInfo;
use crate::terminal::TerminalManager;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

// ============================================================================
// The steward roster
// ============================================================================

/// One steward the runner knows how to launch.
///
/// Every field here was a module-level constant before the roster existed, and
/// **none of them can be derived from the others** — which is why this is a
/// table rather than a naming convention:
///
/// * `enable_env` alternates prefix between skills (`COORD_MERGE_STEWARD_ENABLED`,
///   `COORD_DEVOPS_STEWARD_ENABLED`, but `QONTINUI_CLEANUP_STEWARD_ENABLED`).
/// * `default_mode` is drawn from a **different vocabulary** per skill —
///   `observe`/`autonomous` for two of them, `report`/`reap` for cleanup — so
///   this module deliberately does not validate mode against an enum.
/// * `extra_args` exists because one skill arms its own `/loop` and the others
///   do not; see the field's own doc comment.
pub struct StewardSpec {
    /// Stable identifier and URL path segment (`merge-train`, `dev-ops`, …).
    pub kind: &'static str,
    /// Human-readable label; the UI renders this verbatim.
    pub label: &'static str,
    /// The slash-command this launches, **without** the leading slash. Also
    /// used as the terminal's initial cosmetic title.
    pub skill: &'static str,
    /// The environment variable the skill's own enablement gate reads. The
    /// launch command sets it to `1`; a steward started any other way will
    /// find it unset and refuse to do anything, which is the whole point.
    pub enable_env: &'static str,
    /// Default `--mode=`, matching the skill document's own stated default.
    pub default_mode: &'static str,
    /// Default `/loop` interval, matching the skill document's own stated
    /// default rather than a separately-guessed value.
    pub default_interval: &'static str,
    /// Extra arguments appended after `--mode=`.
    ///
    /// `dev-ops-steward` **arms its own `/loop`** unless told not to, so
    /// wrapping it in `/loop` without `--no-loop` would leave two loops
    /// driving one session — the accumulation hazard that skill's own Step 0
    /// warns about. `merge-train-steward` and `cleanup-steward` do not
    /// self-arm and do not recognise the flag, so it must not be passed to
    /// them: this is per-steward data, not a global.
    pub extra_args: &'static [&'static str],
}

/// Every steward the runner can launch. Adding one is a row here; nothing
/// else in this module is per-steward.
pub const STEWARDS: &[StewardSpec] = &[
    StewardSpec {
        kind: "merge-train",
        label: "Merge-train steward",
        skill: "merge-train-steward",
        enable_env: "COORD_MERGE_STEWARD_ENABLED",
        // CORRECTED 2026-08-28. This was `observe`, with a comment claiming it
        // matched "`merge-train-steward.md`'s own stated default". It has not
        // matched since 2026-07-22, when that skill moved its default to
        // `autonomous` after completing its observe soak; the launcher was
        // never updated and the comment silently became false. Tracking the
        // skill is the point of this table — a launcher that disagrees with
        // the skill it launches is the defect this module was rewritten to
        // end. An observe re-soak — which that skill asks for after a major
        // change — is still reachable, but only over the API
        // (`POST /steward/merge-train/start {"mode":"observe"}`): the UI
        // button sends an empty body and so can only ever launch this default.
        default_mode: "autonomous",
        default_interval: "5m",
        extra_args: &[],
    },
    StewardSpec {
        kind: "dev-ops",
        label: "Dev-ops steward",
        skill: "dev-ops-steward",
        enable_env: "COORD_DEVOPS_STEWARD_ENABLED",
        default_mode: "autonomous",
        default_interval: "10m",
        extra_args: &["--no-loop"],
    },
    StewardSpec {
        kind: "cleanup",
        label: "Cleanup steward",
        skill: "cleanup-steward",
        enable_env: "QONTINUI_CLEANUP_STEWARD_ENABLED",
        // `report` (detect + print only) is this skill's own documented
        // default; `reap` is the mutating mode and is opt-in.
        default_mode: "report",
        default_interval: "15m",
        extra_args: &[],
    },
];

/// Look up a steward by its `kind` path segment.
pub fn steward_spec(kind: &str) -> Option<&'static StewardSpec> {
    STEWARDS.iter().find(|s| s.kind == kind)
}

/// The 404 body for an unrecognised kind — names what IS valid, so a caller
/// that guessed the segment is told the roster rather than just refused.
fn unknown_kind_error(kind: &str) -> (StatusCode, Json<ApiResponse<()>>) {
    let valid: Vec<&str> = STEWARDS.iter().map(|s| s.kind).collect();
    (
        StatusCode::NOT_FOUND,
        Json(api_error(format!(
            "unknown steward kind '{}' (valid: {})",
            kind,
            valid.join(", ")
        ))),
    )
}

// ============================================================================
// In-process steward metadata (kind/mode/interval used at launch)
// ============================================================================

/// What a steward terminal was started as. Keyed by terminal id so
/// `GET /steward/{kind}/status` can echo back what `start` was called with.
/// Runner-local and in-memory only: a runner restart kills the PTY (and thus
/// the steward) anyway, so there's nothing durable to lose.
struct StewardMeta {
    kind: String,
    mode: String,
    interval: String,
}

/// Global registry of steward metadata, keyed by terminal id. Holds at most
/// one live entry per `kind` — enforced by the single-instance guard in
/// [`steward_start_handler`] — but is a flat map rather than a map-of-kind so
/// that stale-entry pruning stays a single `retain` over live terminal ids.
fn steward_meta_store() -> &'static Mutex<HashMap<String, StewardMeta>> {
    static STORE: OnceLock<Mutex<HashMap<String, StewardMeta>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Kinds whose `start` is currently in flight.
///
/// The single-instance guard cannot be enforced by the metadata store alone:
/// that store is keyed by *terminal id*, and there is no id to record until
/// `TerminalManager::create` has already spawned the PTY. Two concurrent
/// `POST /steward/{kind}/start` calls — the UI button and an agent driving
/// the same endpoint by `curl`, say — would therefore both read
/// `running: false`, both spawn, and both insert. The store would then hold
/// two live rows for one kind, `stop` would kill whichever `list()` yielded
/// first, and the survivor would be invisible to `status` and so unstoppable
/// from any surface. This set closes that window by claiming the kind
/// *before* the check.
fn starting_kinds() -> &'static Mutex<std::collections::HashSet<String>> {
    static STARTING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    STARTING.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// An RAII claim on one kind's start slot, released on drop.
///
/// Drop rather than explicit removal because `steward_start_handler` has
/// several early-return paths (the 409, the spawn failure) and a missed
/// removal on any one of them would wedge that kind's start forever — a
/// worse failure than the race it guards.
struct StartClaim(String);

impl StartClaim {
    /// Claim the slot, or `None` if a start of this kind is already in
    /// flight. A poisoned lock also yields `None`: refusing a concurrent
    /// start is the safe direction.
    fn acquire(kind: &str) -> Option<Self> {
        let mut guard = starting_kinds().lock().ok()?;
        if !guard.insert(kind.to_string()) {
            return None;
        }
        Some(StartClaim(kind.to_string()))
    }
}

impl Drop for StartClaim {
    fn drop(&mut self) {
        if let Ok(mut guard) = starting_kinds().lock() {
            guard.remove(&self.0);
        }
    }
}

fn prune_stale_meta(terminal_manager: &TerminalManager) {
    let alive_ids: std::collections::HashSet<String> = terminal_manager
        .list()
        .into_iter()
        .filter(|info| info.is_alive)
        .map(|info| info.id)
        .collect();
    if let Ok(mut guard) = steward_meta_store().lock() {
        guard.retain(|id, _| alive_ids.contains(id));
    }
}

/// The terminal ids recorded for one steward kind.
///
/// Split out of [`find_running_steward`] so the kind-filtering — the part
/// that decides whether one steward's session can be mistaken for another's
/// — is testable without a Tauri `AppHandle` or a live PTY.
fn tracked_ids_for_kind(
    store: &HashMap<String, StewardMeta>,
    kind: &str,
) -> std::collections::HashSet<String> {
    store
        .iter()
        .filter(|(_, meta)| meta.kind == kind)
        .map(|(id, _)| id.clone())
        .collect()
}

/// Find the live terminal session tracked as the steward **of this kind**, if
/// any.
///
/// Keyed on terminal **id** via [`steward_meta_store`], NOT on `title`.
/// Manual testing (2026-07-19, temp-runner UI Bridge verification) found
/// that `title` cannot serve as a durable single-instance marker on this
/// runner: PowerShell (and other shells) emit OSC 0/2 title-change escape
/// sequences, and xterm.js relays those back to the runner via
/// `TerminalSession::set_title` — "Phase 2 of bi-directional title sync"
/// (`terminal/session.rs:1541-1555`) — silently overwriting our title
/// sentinel moments after the shell starts. The skill name is still set at
/// creation as the terminal's initial cosmetic label, but only the
/// metadata-store id membership is authoritative for "running".
fn find_running_steward(terminal_manager: &TerminalManager, kind: &str) -> Option<TerminalInfo> {
    let tracked_ids = steward_meta_store()
        .lock()
        .map(|guard| tracked_ids_for_kind(&guard, kind))
        .unwrap_or_default();
    if tracked_ids.is_empty() {
        return None;
    }
    terminal_manager
        .list()
        .into_iter()
        .find(|info| info.is_alive && tracked_ids.contains(&info.id))
}

// ============================================================================
// Request / Response Types
// ============================================================================

/// Request body for `POST /steward/{kind}/start`.
#[derive(Debug, Deserialize)]
pub struct StewardStartRequest {
    /// Skill mode. The valid values are the launched skill's own, not a
    /// vocabulary this module owns — see [`StewardSpec::default_mode`].
    #[serde(default)]
    pub mode: Option<String>,
    /// `/loop` polling interval (e.g. `"5m"`).
    #[serde(default)]
    pub interval: Option<String>,
}

/// Response for `GET /steward/{kind}/status`, and one element of
/// `GET /stewards`.
#[derive(Debug, Serialize)]
pub struct StewardStatusResponse {
    /// Which steward this row describes.
    pub kind: String,
    pub label: String,
    pub skill: String,
    /// What `start` would use when the caller supplies nothing — surfaced so
    /// a UI can show the cadence it is about to launch without duplicating
    /// this table.
    pub default_mode: String,
    pub default_interval: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    /// Unix timestamp in milliseconds the steward's terminal was created at
    /// (the terminal's own `created_at`, not a separately-tracked value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

/// Compute one steward's status — shared by `GET /steward/{kind}/status`, the
/// `GET /stewards` roster, and the single-instance guard in
/// `POST /steward/{kind}/start`.
fn steward_status(
    terminal_manager: &TerminalManager,
    spec: &'static StewardSpec,
) -> StewardStatusResponse {
    let running = find_running_steward(terminal_manager, spec.kind);
    prune_stale_meta(terminal_manager);

    let base = StewardStatusResponse {
        kind: spec.kind.to_string(),
        label: spec.label.to_string(),
        skill: spec.skill.to_string(),
        default_mode: spec.default_mode.to_string(),
        default_interval: spec.default_interval.to_string(),
        running: false,
        session_id: None,
        mode: None,
        interval: None,
        started_at: None,
    };

    match running {
        Some(info) => {
            let (mode, interval) = steward_meta_store()
                .lock()
                .ok()
                .and_then(|guard| {
                    guard
                        .get(&info.id)
                        .map(|meta| (Some(meta.mode.clone()), Some(meta.interval.clone())))
                })
                .unwrap_or((None, None));

            StewardStatusResponse {
                running: true,
                session_id: Some(info.id.clone()),
                mode,
                interval,
                started_at: Some(info.created_at),
                ..base
            }
        }
        None => base,
    }
}

/// Build the launch command typed into the PTY, platform-appropriate for the
/// env-var prefix. `TerminalSession::build_shell_command` (`terminal/session.rs`)
/// spawns `powershell.exe` on Windows and `$SHELL` (bash by default) on
/// other platforms, so the env-var syntax must match — the same convention
/// already used by the frontend's `buildAiLaunchCommand`
/// (`aiLaunchCommand.ts`): `$env:VAR="value"; cmd` on Windows,
/// `VAR=value cmd` (POSIX prefix form) elsewhere. The plan's literal
/// invocation is POSIX-shaped; this reproduces its effect on both shells
/// rather than typing bash syntax into a PowerShell prompt where it would not
/// parse.
///
/// The `enable_env` assignment is the load-bearing half: every steward skill
/// gates itself on that variable and does nothing at all when it is unset, so
/// a launch command that omitted it would start a session that reports itself
/// disabled on every iteration.
fn build_launch_command(spec: &StewardSpec, mode: &str, interval: &str) -> String {
    let mut base = format!("claude /loop {} /{} --mode={}", interval, spec.skill, mode);
    for arg in spec.extra_args {
        base.push(' ');
        base.push_str(arg);
    }
    if cfg!(target_os = "windows") {
        format!("$env:{}=\"1\"; {}", spec.enable_env, base)
    } else {
        format!("{}=1 {}", spec.enable_env, base)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Get the TerminalManager from Tauri managed state (same helper pattern as
/// `mcp/terminals.rs::get_terminal_manager`).
fn get_terminal_manager(state: &ApiState) -> Arc<TerminalManager> {
    state
        .app_handle
        .state::<Arc<TerminalManager>>()
        .inner()
        .clone()
}

// ============================================================================
// Handlers
// ============================================================================

/// `GET /stewards` — the whole roster with each steward's live status.
///
/// One request answers "what can this runner launch, and what is up right
/// now?", so a polling UI does not have to know the roster in advance or fan
/// out one request per kind.
pub async fn stewards_list_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<StewardStatusResponse>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let rows = STEWARDS
        .iter()
        .map(|spec| steward_status(&terminal_manager, spec))
        .collect();
    Ok(Json(ApiResponse::success(rows)))
}

/// `GET /steward/{kind}/status` — is this steward running, and if so, which
/// terminal/mode/interval?
pub async fn steward_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
) -> Result<Json<ApiResponse<StewardStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager = get_terminal_manager(&state);
    Ok(Json(ApiResponse::success(steward_status(
        &terminal_manager,
        spec,
    ))))
}

/// `POST /steward/{kind}/start` — spawn this steward's PTY terminal session
/// server-side, mirroring `create_terminal_handler`
/// (`mcp/terminals.rs:137`). Refuses with 409 if a steward **of this kind** is
/// already running; different kinds coexist by design.
pub async fn steward_start_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
    Json(request): Json<StewardStartRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager = get_terminal_manager(&state);
    let app_handle = state.app_handle.clone();

    // Claim the start slot BEFORE the running-check, and hold it for the rest
    // of this handler (released on drop). Checking first and claiming later
    // would leave exactly the window this claim exists to close — see
    // [`starting_kinds`].
    let _claim = StartClaim::acquire(spec.kind).ok_or_else(|| {
        warn!(
            "HTTP: Refusing to start {} — another start of this kind is already in flight",
            spec.skill
        );
        (
            StatusCode::CONFLICT,
            Json(api_error(format!("{} is already starting", spec.skill))),
        )
    })?;

    // Single-instance guard, PER KIND: refuse if `GET /steward/{kind}/status`
    // would report running: true. A merge-train steward must not block a
    // dev-ops one.
    let current = steward_status(&terminal_manager, spec);
    if current.running {
        let session_id = current.session_id.unwrap_or_default();
        warn!(
            "HTTP: Refusing to start {} — already running as terminal {}",
            spec.skill, session_id
        );
        return Err((
            StatusCode::CONFLICT,
            Json(api_error(format!(
                "{} is already running (terminal {})",
                spec.skill, session_id
            ))),
        ));
    }

    let mode = request
        .mode
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| spec.default_mode.to_string());
    let interval = request
        .interval
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| spec.default_interval.to_string());
    let launch_command = build_launch_command(spec, &mode, &interval);

    info!(
        "HTTP: Starting {} (mode={}, interval={})",
        spec.skill, mode, interval
    );

    match terminal_manager.create(
        Some(spec.skill.to_string()),
        None, // working_dir — default to workspace root
        None, // page_id — default "default"
        None, // cols — default
        None, // rows — default
        app_handle,
        None, // command override — interactive shell, we type the command in
        // The shared session-env contribution. No isolated edit context here
        // (a steward runs in the shared checkout), so `QONTINUI_SESSION_WORKTREES`
        // is omitted exactly as before — but a steward is an agent session and
        // must still learn where the plans live. See `agent_worktree::session_env`.
        crate::agent_worktree::session_env::session_extra_env(None),
        // UNATTENDED spawn — respect the critical floor. A steward is a
        // long-running autonomous agent session; starting one on a box
        // that is already out of commit is how the incident's `claude`-inside-a-
        // terminal deaths happened. The refusal returns as this endpoint's error
        // body (with lane/headroom/floor), and stewards are explicitly
        // restartable, so a refusal defers the steward rather than losing it.
        false,
    ) {
        Ok(info) => {
            info!("HTTP: Created {} terminal: {}", spec.skill, info.id);

            // Recording the terminal id is what makes this steward reachable
            // by `status` and `stop`. If it fails we must NOT leave the PTY
            // running: an unrecorded steward reports as absent and answers
            // `stop` with a 404, which is precisely the ungovernable-steward
            // failure this module exists to prevent. Close what we just
            // opened and report the failure instead of leaking it.
            // Reduce the lock result to a plain `Result<(), String>` in its own
            // statement. A `PoisonError` carries the `MutexGuard`, which is not
            // `Send`, so holding it across the `.await` below would make this
            // handler's future non-`Send` and it would stop satisfying axum's
            // `Handler` bound — a compile error whose message names the route,
            // not the guard. Scoping the lock here drops it before any await.
            let recorded: Result<(), String> = match steward_meta_store().lock() {
                Ok(mut guard) => {
                    guard.insert(
                        info.id.clone(),
                        StewardMeta {
                            kind: spec.kind.to_string(),
                            mode: mode.clone(),
                            interval: interval.clone(),
                        },
                    );
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            };

            if let Err(detail) = recorded {
                error!(
                    "HTTP: steward metadata store poisoned while registering {} \
                     (terminal {}): {} — closing the terminal rather than leaking \
                     an unstoppable steward",
                    spec.skill, info.id, detail
                );
                let mgr = terminal_manager.clone();
                let orphan = info.id.clone();
                let _ = spawn_blocking_tracked(move || mgr.close(&orphan)).await;
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!(
                        "Failed to register {}: metadata store unavailable",
                        spec.skill
                    ))),
                ));
            }

            // Type the launch command in after a short delay for shell
            // initialization — identical pattern to `create_terminal_handler`'s
            // `initial_command` handling (`mcp/terminals.rs:194-205`).
            //
            // Manual testing (2026-07-19, temp-runner UI Bridge verification)
            // initially misread this as a `$` → `$$` corruption bug: reading
            // the buffer via `?format=text` (ANSI-stripped) visually mangles
            // PSReadLine's per-token syntax-highlighting colors into what
            // looks like a doubled leading character. The raw (base64,
            // un-stripped) buffer confirmed the actual bytes PowerShell
            // receives are byte-for-byte correct, and PowerShell's own
            // `\x1b]633;E;...` shell-integration readback echoes the exact,
            // uncorrupted command line back — this write path has no bug.
            // The "Syntaxfehler" seen in that same manual test traced to an
            // unrelated, pre-existing `claude` PowerShell **function**
            // already defined in the test machine's own `$PROFILE`
            // (confirmed via `Get-Command claude` → `Function`, not the CLI
            // binary) whose own body has a syntax error — an environment
            // issue on that machine, out of scope for this plan.
            let mgr = terminal_manager.clone();
            let tid = info.id.clone();
            let cmd = launch_command.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                if let Some(session) = mgr.get(&tid) {
                    let cmd = format!("{}\r\n", cmd);
                    let _ = session.write(cmd.as_bytes());
                }
            });

            Ok(Json(ApiResponse::success(serde_json::json!({
                "id": info.id,
                "kind": spec.kind,
                "title": spec.skill,
                "mode": mode,
                "interval": interval,
            }))))
        }
        Err(e) => {
            error!("HTTP: Failed to start {}: {}", spec.skill, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to start {}: {}", spec.skill, e))),
            ))
        }
    }
}

/// `POST /steward/{kind}/stop` — stop this steward's tracked terminal
/// session. Reuses the existing `TerminalManager::close` kill path
/// (`terminal/manager.rs:221`, same as `close_terminal_handler`,
/// `mcp/terminals.rs:387`).
pub async fn steward_stop_handler(
    State(state): State<Arc<ApiState>>,
    Path(kind): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let spec = steward_spec(&kind).ok_or_else(|| unknown_kind_error(&kind))?;
    let terminal_manager = get_terminal_manager(&state);

    let info = find_running_steward(&terminal_manager, spec.kind).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("{} is not running", spec.skill))),
        )
    })?;

    let manager = terminal_manager.clone();
    let terminal_id = info.id.clone();

    info!("HTTP: Stopping {} (terminal {})", spec.skill, terminal_id);

    spawn_blocking_tracked(move || manager.close(&terminal_id))
        .await
        .map_err(|e| {
            error!(
                "HTTP: spawn_blocking error closing {} terminal: {}",
                spec.skill, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?
        .map_err(|e| {
            error!("HTTP: Failed to stop {}: {}", spec.skill, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to stop {}: {}", spec.skill, e))),
            )
        })?;

    if let Ok(mut guard) = steward_meta_store().lock() {
        guard.remove(&info.id);
    }

    Ok(Json(ApiResponse::success(serde_json::json!({
        "stopped": true,
        "kind": spec.kind,
        "session_id": info.id,
    }))))
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/stewards", get(stewards_list_handler))
        .route("/steward/{kind}/status", get(steward_status_handler))
        .route("/steward/{kind}/start", post(steward_start_handler))
        .route("/steward/{kind}/stop", post(steward_stop_handler))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert that `values` has no duplicates, naming the offender.
    fn assert_unique(values: impl Iterator<Item = &'static str>, field: &str) {
        let mut seen: Vec<&str> = values.collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "duplicate {} in the steward roster — remaining: {:?}",
            field,
            seen
        );
    }

    #[test]
    fn roster_kinds_are_unique() {
        // `kind` is both the routing key and the single-instance key, so a
        // duplicate silently makes one steward unreachable.
        assert_unique(STEWARDS.iter().map(|s| s.kind), "kind");
    }

    #[test]
    fn roster_enablement_variables_are_unique() {
        // The highest-consequence data error in this table is a copy-paste
        // that gives two stewards the SAME enablement variable: the launch
        // command would then set a flag the started skill does not read, so
        // that steward runs and refuses to act on every iteration — which is
        // indistinguishable from a healthy idle watch, and is the exact bug
        // this roster was introduced to fix. Uniqueness is checked separately
        // from `kind` because the leak-check in
        // `launch_command_sets_the_skills_own_enablement_variable` skips any
        // pair whose variables are equal, so without this assertion that
        // error would pass every other test in this file.
        assert_unique(STEWARDS.iter().map(|s| s.enable_env), "enable_env");
        assert_unique(STEWARDS.iter().map(|s| s.skill), "skill");
    }

    #[test]
    fn roster_matches_the_skill_documents() {
        // A SECOND, INDEPENDENT copy of the three variable names, transcribed
        // from the skill documents that own them:
        //   merge-train-steward.md:84   COORD_MERGE_STEWARD_ENABLED
        //   dev-ops-steward.md:157      COORD_DEVOPS_STEWARD_ENABLED
        //   cleanup-steward.md:63       QONTINUI_CLEANUP_STEWARD_ENABLED
        // Everything else in this file derives its expectations from STEWARDS
        // itself and so cannot fail on a wrong name. This can.
        let expected: &[(&str, &str, &str, &str)] = &[
            (
                "merge-train",
                "COORD_MERGE_STEWARD_ENABLED",
                "autonomous",
                "5m",
            ),
            (
                "dev-ops",
                "COORD_DEVOPS_STEWARD_ENABLED",
                "autonomous",
                "10m",
            ),
            (
                "cleanup",
                "QONTINUI_CLEANUP_STEWARD_ENABLED",
                "report",
                "15m",
            ),
        ];
        assert_eq!(
            STEWARDS.len(),
            expected.len(),
            "roster changed size — update this transcription from the skill docs"
        );
        for (kind, env, mode, interval) in expected {
            let spec = steward_spec(kind).unwrap_or_else(|| panic!("{} missing from roster", kind));
            assert_eq!(spec.enable_env, *env, "{} enablement variable", kind);
            assert_eq!(spec.default_mode, *mode, "{} default mode", kind);
            assert_eq!(
                spec.default_interval, *interval,
                "{} default interval",
                kind
            );
        }
    }

    #[test]
    fn tracked_ids_for_kind_does_not_mix_kinds() {
        // The per-kind single-instance guard is only correct if this filter
        // is: a merge-train session leaking into dev-ops' id set would let
        // `stop` kill the wrong steward.
        let mut store: HashMap<String, StewardMeta> = HashMap::new();
        for (id, kind) in [
            ("term-a", "merge-train"),
            ("term-b", "dev-ops"),
            ("term-c", "dev-ops"),
        ] {
            store.insert(
                id.to_string(),
                StewardMeta {
                    kind: kind.to_string(),
                    mode: "autonomous".to_string(),
                    interval: "5m".to_string(),
                },
            );
        }

        let merge = tracked_ids_for_kind(&store, "merge-train");
        assert_eq!(merge.len(), 1);
        assert!(merge.contains("term-a"));

        let dev_ops = tracked_ids_for_kind(&store, "dev-ops");
        assert_eq!(dev_ops.len(), 2);
        assert!(dev_ops.contains("term-b") && dev_ops.contains("term-c"));
        assert!(
            !dev_ops.contains("term-a"),
            "dev-ops picked up a merge-train terminal"
        );

        // A kind with no rows must be empty, not everything.
        assert!(tracked_ids_for_kind(&store, "cleanup").is_empty());
    }

    #[test]
    fn start_claim_excludes_a_concurrent_start_of_the_same_kind_only() {
        let first = StartClaim::acquire("merge-train").expect("first claim");
        assert!(
            StartClaim::acquire("merge-train").is_none(),
            "a second concurrent start of the same kind must be refused"
        );
        // A different kind is unaffected — stewards of different kinds are
        // meant to coexist.
        let other = StartClaim::acquire("dev-ops").expect("a different kind is not blocked");
        drop(other);

        drop(first);
        // Released on drop, so the kind can start again afterwards.
        assert!(
            StartClaim::acquire("merge-train").is_some(),
            "the claim must be released on drop, or that kind can never start again"
        );
    }

    #[test]
    fn spec_lookup_matches_by_kind_and_rejects_unknown() {
        assert_eq!(
            steward_spec("dev-ops").map(|s| s.skill),
            Some("dev-ops-steward")
        );
        assert_eq!(
            steward_spec("merge-train").map(|s| s.skill),
            Some("merge-train-steward")
        );
        // The skill name is NOT the kind; looking one up by skill must miss.
        assert!(steward_spec("dev-ops-steward").is_none());
        assert!(steward_spec("nope").is_none());
    }

    #[test]
    fn launch_command_sets_the_skills_own_enablement_variable() {
        // The discriminating assertion: each steward must get ITS variable,
        // not a shared one. `COORD_MERGE_STEWARD_ENABLED=1 claude
        // /dev-ops-steward` would launch a session whose gate never opens.
        for spec in STEWARDS {
            let cmd = build_launch_command(spec, spec.default_mode, spec.default_interval);
            assert!(
                cmd.contains(spec.enable_env),
                "{} launch command omits {}: {}",
                spec.kind,
                spec.enable_env,
                cmd
            );
            assert!(
                cmd.contains(&format!("/{} ", spec.skill)),
                "{} launch command does not invoke /{}: {}",
                spec.kind,
                spec.skill,
                cmd
            );
            for other in STEWARDS {
                if other.enable_env != spec.enable_env {
                    assert!(
                        !cmd.contains(other.enable_env),
                        "{} launch command leaks {}'s variable: {}",
                        spec.kind,
                        other.kind,
                        cmd
                    );
                }
            }
        }
    }

    #[test]
    fn build_launch_command_is_shell_appropriate() {
        let spec = steward_spec("merge-train").expect("merge-train is in the roster");
        let cmd = build_launch_command(spec, "observe", "5m");
        if cfg!(target_os = "windows") {
            assert_eq!(
                cmd,
                "$env:COORD_MERGE_STEWARD_ENABLED=\"1\"; claude /loop 5m /merge-train-steward --mode=observe"
            );
        } else {
            assert_eq!(
                cmd,
                "COORD_MERGE_STEWARD_ENABLED=1 claude /loop 5m /merge-train-steward --mode=observe"
            );
        }
    }

    #[test]
    fn build_launch_command_honors_custom_mode_and_interval() {
        let spec = steward_spec("merge-train").expect("merge-train is in the roster");
        let cmd = build_launch_command(spec, "autonomous", "10m");
        assert!(cmd.contains("--mode=autonomous"));
        assert!(cmd.contains("/loop 10m"));
    }

    #[test]
    fn only_the_self_arming_steward_is_told_not_to_loop() {
        // `dev-ops-steward` arms its own `/loop`; wrapping it in one without
        // `--no-loop` leaves two loops driving a single session. The other two
        // skills do not recognise the flag at all, so passing it would be an
        // unknown argument rather than a harmless extra.
        let dev_ops = build_launch_command(
            steward_spec("dev-ops").expect("dev-ops is in the roster"),
            "autonomous",
            "10m",
        );
        assert!(dev_ops.ends_with("--no-loop"), "got: {}", dev_ops);

        for kind in ["merge-train", "cleanup"] {
            let spec = steward_spec(kind).expect("in the roster");
            let cmd = build_launch_command(spec, spec.default_mode, spec.default_interval);
            assert!(
                !cmd.contains("--no-loop"),
                "{} got --no-loop: {}",
                kind,
                cmd
            );
        }
    }

    #[test]
    fn unknown_kind_error_names_the_valid_kinds() {
        let (status, Json(body)) = unknown_kind_error("bogus");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.success);

        // Read the MESSAGE, not just the status. An earlier version of this
        // test destructured the body away and so passed against an empty
        // error string — it asserted a property of `STEWARDS` rather than of
        // the function under test.
        let message = body.error.expect("error envelope carries a message");
        assert!(
            message.contains("bogus"),
            "message should quote the offending kind: {}",
            message
        );
        for spec in STEWARDS {
            assert!(
                message.contains(spec.kind),
                "message should name the valid kind '{}' so a caller can \
                 self-correct: {}",
                spec.kind,
                message
            );
        }
    }
}
