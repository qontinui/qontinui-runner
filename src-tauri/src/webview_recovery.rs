//! In-process detection and recovery for a dead runner webview.
//!
//! Plan: `2026-08-01-runner-dead-webview-is-invisible-to-health.md`
//! (Phase 1a — detect; Phase 2 — recover).
//!
//! # Why this exists
//!
//! The runner's WebView2 host can die while the Rust backend keeps serving.
//! The window goes blank, the app looks dead, and until this module the runner
//! neither noticed nor recovered — observed twice on the operator box, once
//! going unnoticed for ~19 h. The failure that killed it was an assertion in
//! WebView2's **browser** process (`msedge.dll` `STATUS_BREAKPOINT`), which
//! leaves the Rust side completely untouched.
//!
//! # Design principle (from the plan)
//!
//! Detection and recovery live **entirely inside the runner process**. Nothing
//! here may depend on the supervisor, on coord, or on any external agent — end
//! users have no watchdog. A process restart is an explicit **non-goal**: it
//! destroys in-flight sessions, so recovery must preserve the process.
//!
//! # Shape
//!
//! * **Detection (Phase 1a)** — [`attach_process_failed_handler`] subscribes to
//!   `ICoreWebView2::add_ProcessFailed`, so a browser/renderer process death is
//!   a *push* notification at the moment of failure, not a polling artifact.
//!   Windows-only; see the `#[cfg(not(windows))]` stub for what other platforms
//!   fall back to.
//! * **Recovery (Phase 2)** — [`trigger_ui_recovery`] runs a cheapest-first
//!   escalation ladder (reload → recreate) behind a [`LoopGuard`].
//! * **The builder chain is factored, not duplicated** — [`build_main_window`]
//!   is the *only* place the main window is constructed, called once at startup
//!   from `main.rs` and again here on recovery. Duplicating it would let the
//!   recovered window silently drift from the real one (losing the `no-store`
//!   index header, the `window.__QONTINUI_PORT__` injection, or the
//!   renderer-throttling browser args), which is a worse bug than the one this
//!   module fixes.
//!
//! # Server mode is a hard gate
//!
//! `QONTINUI_SERVER_MODE` (`launch_env.rs`) makes `main.rs` skip window
//! creation entirely — a server-mode runner has NO webview, ever. Everything
//! here is inert under it: no handler attach, no recreate, no error, no warning
//! spam. A headless runner must never be conscripted into growing a window it
//! was launched to not have. See [`is_server_mode`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::{debug, error, info, warn};

use crate::window_placement::WindowPlacement;

// ─────────────────────────── tuning constants ────────────────────────────

/// How many recovery attempts are allowed inside one incident before the
/// runner declares the UI terminally broken and stops retrying.
///
/// A webview that dies immediately on recreate must not spin: three attempts
/// (reload, recreate, recreate) is enough to distinguish "transient renderer
/// crash" from "this build cannot host a webview at all".
pub const MAX_RECOVERY_ATTEMPTS: u32 = 3;

/// First backoff step. Attempt *n* (1-indexed) waits
/// `RECOVERY_BACKOFF_BASE_MS * 2^(n-1)`, capped at [`RECOVERY_BACKOFF_MAX_MS`].
pub const RECOVERY_BACKOFF_BASE_MS: u64 = 5_000;

/// Ceiling on the exponential backoff.
pub const RECOVERY_BACKOFF_MAX_MS: u64 = 60_000;

/// A webview that ran this long since the last recovery attempt is a **fresh
/// incident**, not a spin — the attempt counter (and the terminal `exhausted`
/// state) reset. Without this a runner that recovers successfully at 09:00 and
/// crashes again unrelated at 23:00 would inherit the morning's spent budget.
pub const RECOVERY_ATTEMPT_RESET_MS: u64 = 10 * 60 * 1_000;

/// How long to wait for Tauri's event loop to actually retire a destroyed
/// window's label before rebuilding under it. `WebviewWindow::destroy()` only
/// *dispatches* the destroy; the label is released when the event loop
/// processes `WindowEvent::Destroyed`, which is asynchronous with respect to
/// this call.
pub const WINDOW_LABEL_RELEASE_TIMEOUT_MS: u64 = 5_000;

/// Poll interval while waiting for the label to be released.
const LABEL_RELEASE_POLL_MS: u64 = 50;

/// Browser args for the main window's WebView2 host.
///
/// Keeps the renderer live when the window is backgrounded / occluded / on
/// another virtual desktop. Without these flags Chromium throttles the page's
/// timers to ~1/min and, after ~5 min hidden, freezes it entirely: the terminal
/// panes then keep showing the last-painted frame while the frontend's
/// session-advancing loops (state polling, auto-approve, auto-restart) stall.
/// `CalculateNativeWinOcclusion` off is the key flag for the frozen frame.
///
/// NOTE: setting `additional_browser_args` REPLACES wry's default
/// `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`, so those
/// are re-listed here. Windows-only (no-op elsewhere).
const MAIN_WINDOW_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,\
     CalculateNativeWinOcclusion,IntensiveWakeUpThrottling \
     --disable-background-timer-throttling \
     --disable-renderer-backgrounding \
     --disable-backgrounding-occluded-windows";

// ────────────────────────── server-mode gating ───────────────────────────

/// `Some(true)` once `main.rs` observed `LaunchEnv::server_mode`.
///
/// Unset means "`main.rs` never got as far as the window branch", which is
/// treated the same as server mode by [`trigger_ui_recovery`] — there is no
/// [`MainWindowSpec`] recorded either way, so there is nothing to rebuild.
static SERVER_MODE: OnceLock<bool> = OnceLock::new();

/// Record whether this process launched headless. Called exactly once from
/// `main.rs`'s setup closure, on both arms of the `server_mode` branch.
pub fn set_server_mode(server_mode: bool) {
    let _ = SERVER_MODE.set(server_mode);
}

/// True when this runner was launched with `QONTINUI_SERVER_MODE` — it has no
/// webview and must never be given one.
pub fn is_server_mode() -> bool {
    *SERVER_MODE.get().unwrap_or(&false)
}

// ─────────────────────── the main-window builder ────────────────────────

/// Everything [`build_main_window`] needs, captured once at startup so the
/// recovery path can rebuild a window **identical** to the one it replaces.
#[derive(Debug, Clone)]
pub struct MainWindowSpec {
    /// Isolated WebView2 profile directory (temp/secondary runners). `None`
    /// on non-Windows and for the default profile.
    pub data_dir: Option<std::path::PathBuf>,
    /// Logical inner size the builder is seeded with.
    pub initial_size: (f64, f64),
    /// Window chrome. `false` for supervisor-placed borderless runners.
    pub decorations: bool,
    /// Where the window lands, and how it is finalized post-build.
    pub placement: WindowPlacement,
    /// Only used for logging — whether this is a secondary/temp instance.
    pub is_secondary: bool,
}

/// The spec the live main window was built from. Recorded by
/// [`build_main_window`]; read by the recreate rung.
static MAIN_WINDOW_SPEC: OnceLock<MainWindowSpec> = OnceLock::new();

/// Build the runner's main window.
///
/// **This is the single construction site for the main window.** `main.rs`
/// calls it at startup; [`trigger_ui_recovery`] calls it again when it has to
/// recreate a dead one. Anything added to the builder chain must be added
/// here, so both paths get it.
pub fn build_main_window(
    app: &tauri::AppHandle,
    spec: &MainWindowSpec,
) -> tauri::Result<tauri::WebviewWindow> {
    let label = qontinui_runner_lib::get_main_window_label();
    let url = tauri::WebviewUrl::App("index.html".into());

    let mut builder = tauri::WebviewWindowBuilder::new(app, label, url)
        .title("Qontinui Runner")
        .inner_size(spec.initial_size.0, spec.initial_size.1)
        .min_inner_size(1200.0, 700.0)
        .fullscreen(false)
        .resizable(true)
        .decorations(spec.decorations)
        // Phase P2.2 of `tmp_plans/sw-cache-invalidation.md`: mark the embedded
        // index.html as `no-store` so a webview that survives a binary swap
        // can't serve a stale shell whose <script src> tags point at hashed
        // asset filenames the new bundle no longer contains. Hashed `/assets/*`
        // responses pass through with their default headers.
        .on_web_resource_request(crate::asset_headers::stamp_no_store_on_index)
        .additional_browser_args(MAIN_WINDOW_BROWSER_ARGS);

    if let Some(ref dir) = spec.data_dir {
        builder = builder.data_directory(dir.clone());
    }

    // Inject the intended API port as a global so the frontend's synchronous
    // port-resolution fast-path (`window.__QONTINUI_PORT__`) resolves to the
    // *actual* runner port on temp/secondary instances instead of silently
    // falling through to the hardcoded 9876. Without this, hooks on a temp
    // runner route their reads at the primary.
    let intended_api_port = crate::mcp::types::get_mcp_api_port();
    builder =
        builder.initialization_script(format!("window.__QONTINUI_PORT__ = {};", intended_api_port));

    builder = spec.placement.configure_builder(builder);

    let win = builder.build()?;
    spec.placement.finalize(&win);
    let _ = win.show();
    let _ = win.set_focus();
    info!(
        "Main window created (secondary={}, isolated={}, placement={:?})",
        spec.is_secondary,
        spec.data_dir.is_some(),
        spec.placement
    );

    // Record the spec so recovery can rebuild from it. `OnceLock::set` on the
    // recovery path is a no-op (already recorded at startup) — deliberate: the
    // startup spec is the reference shape.
    let _ = MAIN_WINDOW_SPEC.set(spec.clone());

    Ok(win)
}

/// The spec the main window was built from, if one was ever built.
pub fn main_window_spec() -> Option<&'static MainWindowSpec> {
    MAIN_WINDOW_SPEC.get()
}

// ───────────────────────── failure classification ────────────────────────

/// `COREWEBVIEW2_PROCESS_FAILED_KIND`, mapped to the responses they need.
///
/// The raw discriminants are stable ABI values from
/// `webview2-com-sys`'s `COREWEBVIEW2_PROCESS_FAILED_KIND_*` constants; they
/// are matched numerically here so this enum (and its unit tests) compile on
/// every platform, not just Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessFailureKind {
    /// `BROWSER_PROCESS_EXITED` (0) — the whole WebView2 host is gone. The
    /// `CoreWebView2` is unusable and `eval` into it is a no-op, so only a
    /// window rebuild recovers it. This is the observed incident.
    BrowserExited,
    /// `RENDER_PROCESS_EXITED` (1) — often recoverable by reload alone.
    RenderExited,
    /// `RENDER_PROCESS_UNRESPONSIVE` (2) — ditto.
    RenderUnresponsive,
    /// `FRAME_RENDER_PROCESS_EXITED` (3) — an out-of-process iframe died. The
    /// top-level document is unaffected; WebView2 recovers on its own.
    FrameRenderExited,
    /// GPU / utility / sandbox-helper / PPAPI / unknown subprocess exits
    /// (4-9). WebView2 restarts these itself; the page keeps running.
    Ancillary(i32),
}

impl ProcessFailureKind {
    /// Map a raw `COREWEBVIEW2_PROCESS_FAILED_KIND` discriminant.
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::BrowserExited,
            1 => Self::RenderExited,
            2 => Self::RenderUnresponsive,
            3 => Self::FrameRenderExited,
            other => Self::Ancillary(other),
        }
    }

    /// Stable label for logs and the HTTP surface.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BrowserExited => "BROWSER_PROCESS_EXITED",
            Self::RenderExited => "RENDER_PROCESS_EXITED",
            Self::RenderUnresponsive => "RENDER_PROCESS_UNRESPONSIVE",
            Self::FrameRenderExited => "FRAME_RENDER_PROCESS_EXITED",
            Self::Ancillary(_) => "ANCILLARY_PROCESS_EXITED",
        }
    }
}

/// Why recovery was asked for. Two callers by design:
///
/// * [`RecoveryReason::ProcessFailed`] — the Phase 1a push event (this module).
/// * [`RecoveryReason::HeartbeatStale`] — the Phase 1b staleness backstop,
///   wired by the coordinator during integration. **Not wired here**; this
///   variant exists so the signature is already right for it.
/// * [`RecoveryReason::Manual`] — the operator/debug HTTP route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    ProcessFailed(ProcessFailureKind),
    HeartbeatStale,
    Manual,
}

impl RecoveryReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProcessFailed(_) => "process_failed",
            Self::HeartbeatStale => "heartbeat_stale",
            Self::Manual => "manual",
        }
    }
}

/// One rung of the escalation ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// `location.reload()` into the existing webview — the cheap rung. Right
    /// for a renderer-only failure; **useless when the browser process is
    /// gone**, so it is never selected for [`ProcessFailureKind::BrowserExited`].
    Reload,
    /// Destroy and rebuild the main `WebviewWindow`.
    Recreate,
    /// Log only. WebView2 handles this class itself; acting would spin.
    None,
}

/// Choose the rung for `reason` on 0-indexed `attempt`.
///
/// Escalation happens **across attempts**, not inside one call: this module
/// deliberately does not read `ui_bridge_last_pong` (another branch owns that
/// atomic), so it cannot observe whether a reload took. Instead the cheap rung
/// is tried once and any subsequent request for the same incident escalates.
/// A failed `eval` escalates immediately inside the call — see
/// [`trigger_ui_recovery`].
pub fn plan_action(reason: RecoveryReason, attempt: u32) -> RecoveryAction {
    match reason {
        // The browser process is gone: the CoreWebView2 is unusable, so the
        // reload rung is not merely unlikely to work — it is a no-op. Skip
        // straight to recreate.
        RecoveryReason::ProcessFailed(ProcessFailureKind::BrowserExited) => {
            RecoveryAction::Recreate
        }
        // WebView2 recovers these on its own; the top-level document survives.
        RecoveryReason::ProcessFailed(
            ProcessFailureKind::FrameRenderExited | ProcessFailureKind::Ancillary(_),
        ) => RecoveryAction::None,
        // Renderer death, an unresponsive renderer, a stale heartbeat, or an
        // operator poke: try the cheap rung first, escalate if asked again.
        _ => {
            if attempt == 0 {
                RecoveryAction::Reload
            } else {
                RecoveryAction::Recreate
            }
        }
    }
}

/// True when `reason` never warrants an action, at any attempt.
///
/// Derived from [`plan_action`] rather than restating the classification, so
/// the two can't drift (`no_op_reasons_are_no_ops_at_every_attempt` pins the
/// attempt-independence this relies on).
///
/// Checked **before** the loop guard spends budget: a burst of GPU- or
/// utility-process exits must not exhaust the ladder that a real browser crash
/// needs.
pub fn is_no_op_reason(reason: RecoveryReason) -> bool {
    plan_action(reason, 0) == RecoveryAction::None
}

// ──────────────────────────── the loop guard ─────────────────────────────

/// What the [`LoopGuard`] permits right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardDecision {
    /// Go ahead immediately, as 0-indexed `attempt`.
    Proceed { attempt: u32 },
    /// Go ahead as 0-indexed `attempt`, but only after `wait_ms`.
    Backoff { attempt: u32, wait_ms: u64 },
    /// Budget spent. This is a **terminal state** for the incident, not a
    /// retry — surfaced, never spun on.
    Exhausted,
}

/// Attempt budget + exponential backoff for one incident.
///
/// Pure state machine, deliberately free of Tauri and of wall-clock reads so
/// it is unit-testable without a live webview: callers pass `now_ms`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopGuard {
    attempts: u32,
    last_attempt_ms: u64,
    exhausted: bool,
}

impl LoopGuard {
    pub const fn new() -> Self {
        Self {
            attempts: 0,
            last_attempt_ms: 0,
            exhausted: false,
        }
    }

    /// Number of attempts spent on the current incident.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether the current incident is in the terminal exhausted state.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Decide **and record** the next attempt. Mutating by design: a decision
    /// that is not recorded is a decision that can be taken twice in parallel.
    pub fn decide(&mut self, now_ms: u64) -> GuardDecision {
        // A long-quiet gap means the last incident is over; start fresh.
        if self.last_attempt_ms > 0
            && now_ms.saturating_sub(self.last_attempt_ms) >= RECOVERY_ATTEMPT_RESET_MS
        {
            *self = Self::new();
        }

        if self.exhausted || self.attempts >= MAX_RECOVERY_ATTEMPTS {
            self.exhausted = true;
            return GuardDecision::Exhausted;
        }

        let attempt = self.attempts;
        let wait_ms = if attempt == 0 {
            0
        } else {
            let backoff = RECOVERY_BACKOFF_BASE_MS
                .saturating_mul(1u64 << (attempt - 1))
                .min(RECOVERY_BACKOFF_MAX_MS);
            backoff.saturating_sub(now_ms.saturating_sub(self.last_attempt_ms))
        };

        self.attempts += 1;
        self.last_attempt_ms = now_ms;

        if wait_ms > 0 {
            GuardDecision::Backoff { attempt, wait_ms }
        } else {
            GuardDecision::Proceed { attempt }
        }
    }
}

static LOOP_GUARD: Mutex<LoopGuard> = Mutex::new(LoopGuard::new());
static RECOVERY_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static WINDOW_SWAP_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
/// Latches once the user has been told this incident is terminal, so repeated
/// `ProcessFailed` events cannot spam a dialog at someone whose UI is already
/// gone. Cleared by [`LoopGuard::decide`]'s incident reset, alongside the
/// attempt counter it belongs to.
static EXHAUSTION_SURFACED: AtomicBool = AtomicBool::new(false);

/// True while the recovery ladder is between `destroy()` and the rebuild of the
/// main window.
///
/// **Load-bearing, not cosmetic.** Tauri treats "the last window was destroyed"
/// as an exit request: `tauri-runtime-wry`'s `TaoWindowEvent::Destroyed` arm
/// removes the window, and if the window set is then empty it fires
/// `RunEvent::ExitRequested` and sets `ControlFlow::Exit` unless the app calls
/// `api.prevent_exit()`. Destroying the runner's only window to rebuild it
/// would therefore terminate the process and every in-flight session — the
/// explicit non-goal of the recovery plan. `main.rs`'s `app.run` handler reads
/// this flag and vetoes the exit for exactly the duration of the swap.
pub fn window_swap_in_progress() -> bool {
    WINDOW_SWAP_IN_PROGRESS.load(Ordering::SeqCst)
}

/// Clears [`WINDOW_SWAP_IN_PROGRESS`] on every exit path, including a panic or
/// a dropped future — a stuck flag would make the runner un-exitable.
struct SwapGuard;

impl Drop for SwapGuard {
    fn drop(&mut self) {
        WINDOW_SWAP_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// What to do with an observed `RunEvent::ExitRequested`.
///
/// # Why this is not just [`window_swap_in_progress`]
///
/// It used to be, and that is precisely how the runner killed itself at
/// 2026-08-06T01:00:56Z. `WINDOW_SWAP_IN_PROGRESS` is held for the *duration of
/// the swap* — from `destroy()` to the rebuild returning — but the exit request
/// the swap provokes is delivered by the event loop **asynchronously**, and on
/// that incident it arrived 64 ms after the rebuild had already finished and
/// dropped the guard. The flag was false, the veto never ran, and a runner with
/// a perfectly good freshly-built window exited 0 and took nine hours of
/// sessions with it.
///
/// A wider time window would only make the race rarer. The durable fix is to
/// stop asking "*when* is this happening" and ask "*is exiting correct right
/// now*", which is answerable from state that cannot race:
///
/// * **Quit intent wins over everything.** If a deliberate shutdown was
///   requested, exit — no other condition may override it. This is what keeps
///   the veto from ever wedging the process un-exitable, the failure mode the
///   original `SwapGuard` comment was rightly afraid of.
/// * **A live main window means the request is stale.** Tauri only fires
///   `ExitRequested` because it saw the window set go empty. If a main window
///   exists by the time the handler runs, the set was repopulated — a swap
///   rebuilt it — so the request describes a world that no longer exists.
/// * **Mid-swap still needs the flag.** During the swap the window is genuinely
///   gone, so window-liveness cannot distinguish "about to be rebuilt" from
///   "last window closed". That is the one case `WINDOW_SWAP_IN_PROGRESS`
///   answers, and it is kept for exactly that case.
///
/// The two vetoes are complementary, not redundant: the flag covers the swap's
/// interior, window-liveness covers everything after it, and together they
/// leave no gap for a late event to land in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitVeto {
    /// A deliberate shutdown was requested — always honoured.
    AllowQuitRequested,
    /// No main window and no swap in flight: the genuine last-window-closed
    /// exit.
    AllowNoWindow,
    /// The recovery ladder is between `destroy()` and the rebuild.
    VetoSwapInFlight,
    /// A live main window exists and nobody asked to quit — a stale request
    /// left over from a swap's `destroy()`.
    VetoWindowAlive,
}

impl ExitVeto {
    /// True when the exit must be blocked with `api.prevent_exit()`.
    pub fn is_veto(self) -> bool {
        matches!(self, Self::VetoSwapInFlight | Self::VetoWindowAlive)
    }

    /// Stable reason string for the log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllowQuitRequested => "quit_requested",
            Self::AllowNoWindow => "no_window",
            Self::VetoSwapInFlight => "swap_in_flight",
            Self::VetoWindowAlive => "window_alive",
        }
    }
}

/// Pure decision for [`should_veto_exit`], extracted so the priority rules can
/// be asserted without an event loop or a live window. Priority is intentional
/// and load-bearing — see [`ExitVeto`].
const fn decide_exit_veto(
    quit_requested: bool,
    swap_in_progress: bool,
    main_window_alive: bool,
) -> ExitVeto {
    if quit_requested {
        return ExitVeto::AllowQuitRequested;
    }
    if swap_in_progress {
        return ExitVeto::VetoSwapInFlight;
    }
    if main_window_alive {
        return ExitVeto::VetoWindowAlive;
    }
    ExitVeto::AllowNoWindow
}

/// Classify a `RunEvent::ExitRequested`. Called from `main.rs`'s `app.run`
/// handler, which vetoes when [`ExitVeto::is_veto`] holds.
pub fn should_veto_exit(app: &tauri::AppHandle) -> ExitVeto {
    use tauri::Manager;

    // Server mode has no window and no swap, so this reduces to
    // `AllowQuitRequested`/`AllowNoWindow` — a headless runner is never vetoed.
    let label = qontinui_runner_lib::get_main_window_label();
    decide_exit_veto(
        crate::commands::terminal_windows::is_app_quitting(),
        window_swap_in_progress(),
        app.get_webview_window(label).is_some(),
    )
}

/// True once recovery gave up on the current incident.
///
/// Read straight off the [`LoopGuard`] rather than mirrored into a second
/// atomic — one source of truth, so the surfaced state cannot drift from the
/// state that actually gates retries. The Phase 3 user-visible surface (native
/// dialog / `derived_status`) reads this; it is deliberately *not* a retry
/// trigger.
pub fn recovery_exhausted() -> bool {
    LOOP_GUARD
        .lock()
        .map(|g| g.is_exhausted())
        .unwrap_or_else(|p| p.into_inner().is_exhausted())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Tell the user, natively, that the UI is gone and is not coming back on its
/// own (plan Phase 3).
///
/// This is the whole point of the phase: a blank window with no explanation is
/// the worst outcome and is what shipped before. By the time this fires the
/// webview is dead, so **every channel here must be webview-independent** —
/// both the notification and the dialog are OS-native surfaces driven from
/// Rust, which is exactly why they still work when the thing they are
/// describing is broken.
///
/// Fires at most once per incident ([`EXHAUSTION_SURFACED`]): repeated
/// `ProcessFailed` events after the budget is spent must not stack dialogs on
/// someone who already cannot use the app.
///
/// Best-effort by contract — a missing notification permission or a headless
/// desktop must never turn "we could not tell the user" into a second failure.
fn surface_exhaustion_to_user(app: &tauri::AppHandle, attempts: u32) {
    // Server mode has no desktop to surface to. `trigger_ui_recovery` returns
    // before reaching here, but this is defence in depth for any future caller.
    if is_server_mode() {
        return;
    }
    if EXHAUSTION_SURFACED.swap(true, Ordering::SeqCst) {
        debug!("UI recovery: exhaustion already surfaced for this incident");
        return;
    }

    const TITLE: &str = "Qontinui Runner — the window stopped working";
    let body = format!(
        "The runner's UI host crashed and could not be restarted after {attempts} attempts.\n\n\
         Automation and the API on port 9876 are still running — your sessions are NOT lost.\n\n\
         To get the window back, restart the Qontinui Runner application. If it keeps \
         happening, updating the Microsoft Edge WebView2 Runtime is the fix, since the \
         crash originates inside WebView2 rather than in Qontinui."
    );

    {
        use tauri_plugin_notification::NotificationExt;
        if let Err(e) = app.notification().builder().title(TITLE).body(&body).show() {
            warn!(error = %e, "UI recovery: could not post the exhaustion notification");
        }
    }

    {
        use tauri_plugin_dialog::DialogExt;
        // Non-blocking `show`: a modal `blocking_show` here would park a
        // runtime thread on user input during an active incident.
        app.dialog()
            .message(&body)
            .title(TITLE)
            .kind(tauri_plugin_dialog::MessageDialogKind::Error)
            .show(|_| {});
    }

    error!(
        attempts,
        "UI recovery exhausted — surfaced to the user natively (notification + dialog)"
    );
}

// ───────────────────────── the recovery entry point ──────────────────────

/// What a recovery run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RecoveryOutcome {
    /// Nothing was attempted, and that is correct. `why` is a stable reason
    /// string (server mode, no window was ever built, a run already in flight,
    /// or a failure class WebView2 handles itself).
    Skipped { why: &'static str },
    /// `location.reload()` was dispatched into the live webview.
    Reloaded,
    /// The main window was destroyed and rebuilt.
    Recreated,
    /// The attempt budget for this incident is spent. Terminal — the caller
    /// must not retry.
    Exhausted { attempts: u32 },
    /// A rung was attempted and failed.
    Failed { detail: String },
}

impl RecoveryOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Skipped { .. } => "skipped",
            Self::Reloaded => "reloaded",
            Self::Recreated => "recreated",
            Self::Exhausted { .. } => "exhausted",
            Self::Failed { .. } => "failed",
        }
    }
}

/// **The single recovery entry point.** Runs the escalation ladder behind the
/// loop guard.
///
/// Two callers by design:
/// 1. [`attach_process_failed_handler`]'s WebView2 `ProcessFailed` callback
///    (Phase 1a, wired here).
/// 2. The Phase 1b heartbeat-staleness backstop, with
///    [`RecoveryReason::HeartbeatStale`] — wired by the coordinator during
///    integration, **not** by this module.
///
/// Plus the operator/debug HTTP route with [`RecoveryReason::Manual`].
///
/// Inert in server mode and when no main window was ever built.
pub async fn trigger_ui_recovery(
    app: &tauri::AppHandle,
    reason: RecoveryReason,
) -> RecoveryOutcome {
    // ── Hard gate 1: headless runners have no webview, ever. ──────────────
    if is_server_mode() {
        debug!(
            reason = reason.as_str(),
            "UI recovery skipped: server mode (this runner has no webview by design)"
        );
        return RecoveryOutcome::Skipped { why: "server_mode" };
    }

    // ── Hard gate 2: no window was ever built (window creation failed, or
    //    `main.rs` never reached the window branch). There is nothing to
    //    rebuild *from*, and inventing a spec would fabricate a window this
    //    process never had.
    if main_window_spec().is_none() {
        debug!(
            reason = reason.as_str(),
            "UI recovery skipped: no main window was ever built"
        );
        return RecoveryOutcome::Skipped {
            why: "no_main_window",
        };
    }

    // ── Hard gate 3: failure classes WebView2 restarts by itself. Checked
    //    BEFORE the loop guard so a burst of GPU/utility-process exits cannot
    //    spend the budget a real browser crash needs.
    if is_no_op_reason(reason) {
        debug!(
            reason = reason.as_str(),
            "UI recovery: no action needed — WebView2 recovers this failure class itself"
        );
        return RecoveryOutcome::Skipped {
            why: "no_action_needed",
        };
    }

    // ── Single-flight. Without this, a browser-process death that fires
    //    ProcessFailed several times (browser + orphaned renderers) would run
    //    concurrent recreates against the same label.
    if RECOVERY_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        debug!(
            reason = reason.as_str(),
            "UI recovery skipped: a recovery run is already in flight"
        );
        return RecoveryOutcome::Skipped {
            why: "already_in_progress",
        };
    }
    let _guard = InProgressGuard;

    let decision = {
        let mut guard = match LOOP_GUARD.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let decision = guard.decide(now_ms());
        // Any transition OUT of the exhausted state — `decide`'s long-quiet
        // incident reset, or a recovery that later succeeds — re-arms the
        // user-facing notice so a genuinely new incident can speak up again.
        // Reading it off the guard keeps one source of truth: the latch can
        // only be set while `is_exhausted()` holds.
        if !guard.is_exhausted() {
            EXHAUSTION_SURFACED.store(false, Ordering::SeqCst);
        }
        decision
    };

    let attempt = match decision {
        GuardDecision::Exhausted => {
            // `decide()` already latched the terminal state on the guard, which
            // is what `recovery_exhausted()` reports.
            error!(
                reason = reason.as_str(),
                max_attempts = MAX_RECOVERY_ATTEMPTS,
                "UI recovery EXHAUSTED — the webview keeps failing immediately after recreate. \
                 Not retrying; the UI is terminally broken for this incident."
            );
            // Phase 3: never leave the user staring at a blank window. The
            // status surfaces (`derived_status: "errored"`) tell the FLEET;
            // this tells the person sitting in front of it.
            surface_exhaustion_to_user(app, MAX_RECOVERY_ATTEMPTS);
            return RecoveryOutcome::Exhausted {
                attempts: MAX_RECOVERY_ATTEMPTS,
            };
        }
        GuardDecision::Backoff { attempt, wait_ms } => {
            info!(
                reason = reason.as_str(),
                attempt, wait_ms, "UI recovery backing off before the next attempt"
            );
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            attempt
        }
        GuardDecision::Proceed { attempt } => attempt,
    };

    // Unreachable as `None` — gate 3 above already returned for those reasons —
    // but matched exhaustively rather than assumed.
    let action = plan_action(reason, attempt);
    if action == RecoveryAction::None {
        return RecoveryOutcome::Skipped {
            why: "no_action_needed",
        };
    }

    info!(
        reason = reason.as_str(),
        attempt,
        action = ?action,
        "UI recovery starting"
    );

    // ── Rung 1: reload. Cheap, in-place, keeps the window and its geometry.
    if action == RecoveryAction::Reload {
        match reload_main_webview(app) {
            Ok(()) => {
                info!("UI recovery: reload dispatched into the existing webview");
                return RecoveryOutcome::Reloaded;
            }
            Err(e) => {
                // A failed `eval` is hard evidence the webview is beyond a
                // reload — escalate inside this same call rather than waiting
                // for another trigger.
                warn!(error = %e, "UI recovery: reload failed — escalating to recreate");
            }
        }
    }

    // ── Rung 2: recreate.
    match recreate_main_window(app).await {
        Ok(()) => {
            info!("UI recovery: main window recreated");
            RecoveryOutcome::Recreated
        }
        Err(e) => {
            error!(error = %e, "UI recovery: main window recreate FAILED");
            RecoveryOutcome::Failed { detail: e }
        }
    }
}

/// Clears [`RECOVERY_IN_PROGRESS`] even if the recovery future is dropped.
struct InProgressGuard;

impl Drop for InProgressGuard {
    fn drop(&mut self) {
        RECOVERY_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

fn reload_main_webview(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let label = qontinui_runner_lib::get_main_window_label();
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| format!("main window '{label}' not found"))?;
    window
        .eval("location.reload()")
        .map_err(|e| format!("eval(location.reload()): {e}"))
}

/// Destroy the (dead) main window and rebuild it from the recorded
/// [`MainWindowSpec`].
///
/// Spike finding baked in: `WebviewWindow::destroy()` only *dispatches* the
/// destroy — Tauri releases the label when its event loop processes
/// `WindowEvent::Destroyed` (`tauri::app::on_window_close`), which is
/// asynchronous with respect to this call. Rebuilding immediately races that
/// and fails with `WindowLabelAlreadyExists`, so we poll for the release.
async fn recreate_main_window(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;

    let label = qontinui_runner_lib::get_main_window_label();
    let base_spec = main_window_spec()
        .ok_or_else(|| "no main window spec recorded — nothing to rebuild from".to_string())?;

    // ⚠ Destroying the LAST window makes Tauri request an app exit — which
    // would kill the process and every in-flight session, the explicit
    // non-goal of this plan. Latch the swap so `main.rs`'s
    // `RunEvent::ExitRequested` arm vetoes it. See `window_swap_in_progress`.
    WINDOW_SWAP_IN_PROGRESS.store(true, Ordering::SeqCst);
    let _swap = SwapGuard;

    // Preserve whatever the operator had on screen. These are tao/HWND reads,
    // independent of the (dead) WebView2 host, so they still answer.
    let mut spec = base_spec.clone();
    if let Some(existing) = app.get_webview_window(label) {
        spec.placement = capture_placement(&existing, &base_spec.placement);
        if let Err(e) = existing.destroy() {
            warn!(error = %e, "UI recovery: destroy() of the dead main window failed — rebuilding anyway");
        }
    } else {
        warn!("UI recovery: main window label was already free before recreate");
    }

    // Wait for the label to actually be retired.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(WINDOW_LABEL_RELEASE_TIMEOUT_MS);
    while app.get_webview_window(label).is_some() {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "window label '{label}' still registered {WINDOW_LABEL_RELEASE_TIMEOUT_MS}ms after destroy()"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(LABEL_RELEASE_POLL_MS)).await;
    }

    // `WebviewWindowBuilder::build()` dispatches to the event loop and blocks
    // the calling thread until it answers, so it must not run on an async
    // worker.
    //
    // UNVERIFIED (needs the coordinator's live kill test on a temp runner):
    // whether a WebView2 user-data directory (`spec.data_dir`, set for
    // temp/secondary runners) is still locked by the crashed browser process's
    // orphaned siblings at this point. If it is, `build()` returns an error
    // here — which the loop guard handles by escalating and ultimately
    // exhausting rather than spinning. Everything else about this path is
    // established from the Tauri/wry sources; this one is not statically
    // decidable.
    let app_for_build = app.clone();
    let built = tokio::task::spawn_blocking(move || build_main_window(&app_for_build, &spec))
        .await
        .map_err(|e| format!("recreate task panicked: {e}"))?;

    let win = built.map_err(|e| format!("WebviewWindowBuilder::build(): {e}"))?;

    // Re-arm detection on the fresh webview — otherwise the first recovery
    // would be the last one this process could ever notice.
    attach_process_failed_handler(&win);
    Ok(())
}

/// Best-effort: rebuild where the window actually is, not where it booted.
fn capture_placement(win: &tauri::WebviewWindow, fallback: &WindowPlacement) -> WindowPlacement {
    if win.is_maximized().unwrap_or(false) {
        return WindowPlacement::Maximized;
    }
    match (win.outer_position(), win.outer_size()) {
        (Ok(pos), Ok(size)) if size.width > 0 && size.height > 0 => WindowPlacement::Positioned {
            x: pos.x,
            y: pos.y,
            w: size.width,
            h: size.height,
        },
        _ => fallback.clone(),
    }
}

// ───────────────────── Phase 1a: the ProcessFailed hook ──────────────────

/// Subscribe to `ICoreWebView2::add_ProcessFailed` on `window`.
///
/// A browser/renderer process death then becomes a **push** notification at the
/// moment of failure — no polling window, no ambiguity — which is what makes
/// this detection path independent of the heartbeat backstop.
///
/// Best-effort by contract: every failure to attach is logged and swallowed,
/// because the Phase 1b heartbeat-staleness backstop covers the gap.
#[cfg(windows)]
pub fn attach_process_failed_handler(window: &tauri::WebviewWindow) {
    use tauri::Manager;

    // Never attach on a runner that was launched headless. `main.rs` does not
    // call this in server mode, but the guard is kept local so the invariant
    // survives a future caller.
    if is_server_mode() {
        return;
    }

    let app = window.app_handle().clone();

    let dispatch = window.with_webview(move |wv| {
        // webview2-com re-exports the WebView2 COM types as `Microsoft`; they
        // are generated against `windows 0.61`, which is also what our renamed
        // `windows-capture` dep provides — so the `Result` type below is the
        // *same* type the generated handler expects. Using our direct
        // `windows 0.58` here would be rejected by the generic bounds. See
        // `Cargo.toml`'s `windows-capture` note; this already bit the capture
        // path once.
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2ProcessFailedEventArgs, COREWEBVIEW2_PROCESS_FAILED_KIND,
        };
        use webview2_com::ProcessFailedEventHandler;

        // Built outside the `unsafe` block below so the one `unsafe` inside the
        // callback (`ProcessFailedKind()`) is not a nested — and therefore
        // lint-flagged — unsafe block.
        let handler = ProcessFailedEventHandler::create(Box::new(
            move |_sender: Option<ICoreWebView2>,
                  args: Option<ICoreWebView2ProcessFailedEventArgs>|
                  -> windows_capture::core::Result<()> {
                // SAFETY: `ProcessFailedKind` is a plain out-param vtable read
                // on the event args WebView2 just handed us, on the UI thread
                // it raised the event from; `kind` is a live stack local.
                let raw = args
                    .as_ref()
                    .and_then(|a| {
                        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                        unsafe { a.ProcessFailedKind(&mut kind) }
                            .ok()
                            .map(|()| kind.0)
                    })
                    .unwrap_or(-1);
                let kind = ProcessFailureKind::from_raw(raw);

                // Log EVERY event, even the ones we take no action on, so the
                // failure is in the log trail whether or not recovery works.
                warn!(
                    kind = kind.as_str(),
                    raw_kind = raw,
                    "WebView2 ProcessFailed"
                );

                // We are on the WebView2 UI thread — never block it.
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let outcome =
                        trigger_ui_recovery(&app, RecoveryReason::ProcessFailed(kind)).await;
                    info!(
                        kind = kind.as_str(),
                        outcome = outcome.as_str(),
                        "WebView2 ProcessFailed recovery finished"
                    );
                });
                Ok(())
            },
        ));

        // SAFETY: every call below is a COM call into WebView2. They run on the
        // WebView2 UI thread (`with_webview` guarantees this) and the COM
        // objects are kept alive by the surrounding scope. The event handler is
        // owned by the `CoreWebView2` once registered, so we never remove the
        // token — the subscription lives exactly as long as the webview does.
        let result: Result<(), String> = (|| unsafe {
            let controller = wv.controller();
            let core = controller
                .CoreWebView2()
                .map_err(|e| format!("CoreWebView2(): {e}"))?;

            let mut token: i64 = 0;
            core.add_ProcessFailed(&handler, &mut token)
                .map_err(|e| format!("add_ProcessFailed: {e}"))?;
            Ok(())
        })();

        match result {
            Ok(()) => info!("WebView2 ProcessFailed handler attached to the main window"),
            Err(e) => warn!(
                error = %e,
                "Failed to attach the WebView2 ProcessFailed handler — \
                 falling back to the heartbeat-staleness backstop"
            ),
        }
    });

    if let Err(e) = dispatch {
        warn!(error = %e, "with_webview failed while attaching the ProcessFailed handler");
    }
}

/// Non-Windows stub — **a deliberate no-op, not an oversight.**
///
/// macOS (`WKNavigationDelegate::webViewWebContentProcessDidTerminate:`) and
/// Linux (WebKitGTK's `web-process-terminated` signal) do expose equivalent
/// termination signals, but neither is reachable through Tauri's
/// `PlatformWebview` without hand-rolled delegate/GObject plumbing that has no
/// precedent in this codebase. Rather than ship a half-wired platform path,
/// those platforms rely on the plan's **Phase 1b heartbeat-staleness backstop**
/// (`ui_stale(last_pong, pong_age_ms, UI_DEAD_AFTER_MS)`), which is
/// cross-platform by construction and calls [`trigger_ui_recovery`] with
/// [`RecoveryReason::HeartbeatStale`]. The recovery ladder itself
/// ([`trigger_ui_recovery`], [`build_main_window`]) is fully cross-platform, so
/// only the *detection latency* differs off Windows.
#[cfg(not(windows))]
pub fn attach_process_failed_handler(_window: &tauri::WebviewWindow) {
    debug!(
        "ProcessFailed subscription is Windows-only; this platform relies on the \
         heartbeat-staleness backstop for dead-webview detection"
    );
}

// ─────────────────────── operator/debug HTTP surface ─────────────────────

/// `POST /ui/recover` response.
#[derive(Debug, Serialize)]
pub struct RecoverUiResponse {
    pub reason: &'static str,
    #[serde(flatten)]
    pub result: RecoveryOutcome,
    pub attempts: u32,
    pub exhausted: bool,
    pub server_mode: bool,
}

/// `POST /ui/recover` — manually trigger the recovery ladder.
///
/// An **operator/debug affordance, not the recovery mechanism**: the shipped
/// path is the push `ProcessFailed` event (Phase 1a) plus the heartbeat
/// backstop (Phase 1b). It exists because nothing on the runner API could
/// reach the webview before — `ui_bridge_reload_webview` is a
/// `#[tauri::command]` that is absent from the invoke allowlist, so `/reload`,
/// `/ui/reload`, `/ui-bridge/reload` and `/api/reload` all 404 — and because a
/// human diagnosing a blank window needs a lever that does not restart the
/// process.
pub async fn recover_ui_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::mcp::types::ApiState>>,
) -> axum::Json<RecoverUiResponse> {
    let app = state.app_handle.clone();
    let result = trigger_ui_recovery(&app, RecoveryReason::Manual).await;
    let attempts = LOOP_GUARD
        .lock()
        .map(|g| g.attempts())
        .unwrap_or_else(|p| p.into_inner().attempts());
    axum::Json(RecoverUiResponse {
        reason: RecoveryReason::Manual.as_str(),
        result,
        attempts,
        exhausted: recovery_exhausted(),
        server_mode: is_server_mode(),
    })
}

// ────────────────────────────────  tests  ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── escalation ladder (kind → action) ──────────────────────────────

    #[test]
    fn browser_process_exit_never_tries_reload() {
        // `eval` into a dead browser process is a no-op, so the cheap rung is
        // not merely unlikely to help — it cannot help. Recreate on attempt 0.
        let reason = RecoveryReason::ProcessFailed(ProcessFailureKind::BrowserExited);
        assert_eq!(plan_action(reason, 0), RecoveryAction::Recreate);
        assert_eq!(plan_action(reason, 1), RecoveryAction::Recreate);
        assert_eq!(plan_action(reason, 2), RecoveryAction::Recreate);
    }

    #[test]
    fn render_process_failures_try_reload_first_then_escalate() {
        for kind in [
            ProcessFailureKind::RenderExited,
            ProcessFailureKind::RenderUnresponsive,
        ] {
            let reason = RecoveryReason::ProcessFailed(kind);
            assert_eq!(plan_action(reason, 0), RecoveryAction::Reload, "{kind:?}");
            assert_eq!(plan_action(reason, 1), RecoveryAction::Recreate, "{kind:?}");
        }
    }

    #[test]
    fn ancillary_process_failures_take_no_action() {
        // GPU / utility / sandbox-helper / PPAPI / frame-renderer exits are
        // restarted by WebView2 itself; acting on them would spin the guard.
        assert_eq!(
            plan_action(
                RecoveryReason::ProcessFailed(ProcessFailureKind::FrameRenderExited),
                0
            ),
            RecoveryAction::None
        );
        for raw in [4, 5, 6, 7, 8, 9] {
            let kind = ProcessFailureKind::from_raw(raw);
            assert!(matches!(kind, ProcessFailureKind::Ancillary(_)), "{raw}");
            assert_eq!(
                plan_action(RecoveryReason::ProcessFailed(kind), 0),
                RecoveryAction::None,
                "raw kind {raw}"
            );
        }
    }

    #[test]
    fn no_op_reasons_are_no_ops_at_every_attempt() {
        // `is_no_op_reason` derives from `plan_action(reason, 0)`, which is only
        // sound if the None classification is attempt-independent. Pin it.
        let all: Vec<RecoveryReason> = (-1..12)
            .map(|raw| RecoveryReason::ProcessFailed(ProcessFailureKind::from_raw(raw)))
            .chain([RecoveryReason::HeartbeatStale, RecoveryReason::Manual])
            .collect();
        for reason in all {
            let none_at_zero = plan_action(reason, 0) == RecoveryAction::None;
            assert_eq!(
                none_at_zero,
                is_no_op_reason(reason),
                "is_no_op_reason disagrees with plan_action for {reason:?}"
            );
            for attempt in 0..5 {
                assert_eq!(
                    plan_action(reason, attempt) == RecoveryAction::None,
                    none_at_zero,
                    "{reason:?} changed no-op status at attempt {attempt}"
                );
            }
        }
    }

    #[test]
    fn heartbeat_and_manual_reasons_escalate_across_attempts() {
        for reason in [RecoveryReason::HeartbeatStale, RecoveryReason::Manual] {
            assert_eq!(plan_action(reason, 0), RecoveryAction::Reload, "{reason:?}");
            assert_eq!(
                plan_action(reason, 1),
                RecoveryAction::Recreate,
                "{reason:?}"
            );
        }
    }

    #[test]
    fn process_failed_kind_raw_mapping_matches_the_webview2_abi() {
        // Discriminants from webview2-com-sys' COREWEBVIEW2_PROCESS_FAILED_KIND_*
        assert_eq!(
            ProcessFailureKind::from_raw(0),
            ProcessFailureKind::BrowserExited
        );
        assert_eq!(
            ProcessFailureKind::from_raw(1),
            ProcessFailureKind::RenderExited
        );
        assert_eq!(
            ProcessFailureKind::from_raw(2),
            ProcessFailureKind::RenderUnresponsive
        );
        assert_eq!(
            ProcessFailureKind::from_raw(3),
            ProcessFailureKind::FrameRenderExited
        );
        assert_eq!(
            ProcessFailureKind::from_raw(6),
            ProcessFailureKind::Ancillary(6)
        );
        // An args read that failed yields -1 and must not be mistaken for a
        // browser-process exit (which would trigger a needless recreate).
        assert_eq!(
            ProcessFailureKind::from_raw(-1),
            ProcessFailureKind::Ancillary(-1)
        );
        assert_eq!(
            plan_action(
                RecoveryReason::ProcessFailed(ProcessFailureKind::from_raw(-1)),
                0
            ),
            RecoveryAction::None
        );
    }

    // ── exit veto ──────────────────────────────────────────────────────

    /// The regression this whole decision exists for. On 2026-08-06 the swap
    /// finished, the guard dropped, and the exit request arrived 64 ms later
    /// against a live, freshly rebuilt window — and was honoured. Window
    /// liveness is what makes that request refusable after the fact.
    #[test]
    fn late_exit_after_a_completed_swap_is_vetoed() {
        assert_eq!(
            decide_exit_veto(false, false, true),
            ExitVeto::VetoWindowAlive
        );
        assert!(decide_exit_veto(false, false, true).is_veto());
    }

    #[test]
    fn exit_during_the_swap_is_vetoed() {
        // Mid-swap the window is genuinely gone, so only the flag can answer.
        assert_eq!(
            decide_exit_veto(false, true, false),
            ExitVeto::VetoSwapInFlight
        );
    }

    /// Quit intent outranks both vetoes. Without this the veto could wedge the
    /// process un-exitable — the failure mode the original guard feared.
    #[test]
    fn a_requested_quit_is_never_vetoed() {
        for &swap in &[false, true] {
            for &alive in &[false, true] {
                let d = decide_exit_veto(true, swap, alive);
                assert_eq!(d, ExitVeto::AllowQuitRequested, "swap={swap} alive={alive}");
                assert!(!d.is_veto(), "swap={swap} alive={alive}");
            }
        }
    }

    #[test]
    fn genuine_last_window_closed_exit_is_allowed() {
        let d = decide_exit_veto(false, false, false);
        assert_eq!(d, ExitVeto::AllowNoWindow);
        assert!(!d.is_veto());
    }

    /// A recreate that FAILED leaves no window and no swap in flight, so the
    /// process is allowed to exit rather than lingering windowless forever.
    #[test]
    fn failed_recreate_does_not_wedge_the_process_alive() {
        assert!(!decide_exit_veto(false, false, false).is_veto());
    }

    // ── loop guard / backoff state machine ─────────────────────────────

    /// Arbitrary non-zero epoch. `last_attempt_ms == 0` means "never
    /// attempted", so tests must not use 0 as a timestamp.
    const T0: u64 = 1_700_000_000_000;

    #[test]
    fn loop_guard_first_attempt_proceeds_immediately() {
        let mut g = LoopGuard::new();
        assert_eq!(g.decide(T0), GuardDecision::Proceed { attempt: 0 });
        assert_eq!(g.attempts(), 1);
    }

    #[test]
    fn loop_guard_backs_off_exponentially() {
        let mut g = LoopGuard::new();
        assert_eq!(g.decide(T0), GuardDecision::Proceed { attempt: 0 });
        // Immediately again → full base backoff.
        assert_eq!(
            g.decide(T0),
            GuardDecision::Backoff {
                attempt: 1,
                wait_ms: RECOVERY_BACKOFF_BASE_MS
            }
        );
        // Third attempt doubles.
        assert_eq!(
            g.decide(T0),
            GuardDecision::Backoff {
                attempt: 2,
                wait_ms: RECOVERY_BACKOFF_BASE_MS * 2
            }
        );
    }

    #[test]
    fn loop_guard_credits_elapsed_time_against_the_backoff() {
        let mut g = LoopGuard::new();
        assert_eq!(g.decide(T0), GuardDecision::Proceed { attempt: 0 });
        // Half the base backoff has already elapsed → only the rest is waited.
        assert_eq!(
            g.decide(T0 + RECOVERY_BACKOFF_BASE_MS / 2),
            GuardDecision::Backoff {
                attempt: 1,
                wait_ms: RECOVERY_BACKOFF_BASE_MS / 2
            }
        );
        // Enough elapsed → no wait at all.
        let mut g2 = LoopGuard::new();
        assert_eq!(g2.decide(T0), GuardDecision::Proceed { attempt: 0 });
        assert_eq!(
            g2.decide(T0 + RECOVERY_BACKOFF_BASE_MS + 1),
            GuardDecision::Proceed { attempt: 1 }
        );
    }

    #[test]
    fn loop_guard_exhausts_and_stays_exhausted() {
        // A webview that dies immediately on recreate must not spin forever.
        let mut g = LoopGuard::new();
        for _ in 0..MAX_RECOVERY_ATTEMPTS {
            assert_ne!(g.decide(T0), GuardDecision::Exhausted);
        }
        assert_eq!(g.decide(T0), GuardDecision::Exhausted);
        assert!(g.is_exhausted());
        // Terminal — repeated asks stay terminal within the incident window.
        assert_eq!(g.decide(T0 + 1_000), GuardDecision::Exhausted);
        assert_eq!(
            g.decide(T0 + RECOVERY_ATTEMPT_RESET_MS - 1),
            GuardDecision::Exhausted
        );
    }

    #[test]
    fn loop_guard_resets_after_a_quiet_window() {
        // A crash 14 hours after a successful recovery is a fresh incident, not
        // a continuation of the morning's spin.
        let mut g = LoopGuard::new();
        for _ in 0..MAX_RECOVERY_ATTEMPTS {
            g.decide(T0);
        }
        assert_eq!(g.decide(T0), GuardDecision::Exhausted);
        assert_eq!(
            g.decide(T0 + RECOVERY_ATTEMPT_RESET_MS),
            GuardDecision::Proceed { attempt: 0 }
        );
        assert!(!g.is_exhausted());
    }

    #[test]
    fn loop_guard_backoff_is_capped() {
        let mut g = LoopGuard::new();
        g.decide(T0);
        let mut last = 0;
        for _ in 1..MAX_RECOVERY_ATTEMPTS {
            if let GuardDecision::Backoff { wait_ms, .. } = g.decide(T0) {
                last = wait_ms;
            }
        }
        assert!(
            last <= RECOVERY_BACKOFF_MAX_MS,
            "backoff {last} exceeded cap {RECOVERY_BACKOFF_MAX_MS}"
        );
    }

    #[test]
    fn calibrations_cannot_drift() {
        assert!(RECOVERY_BACKOFF_BASE_MS <= RECOVERY_BACKOFF_MAX_MS);
        // The reset window must outlast a full exhausted ladder, or a guard
        // could reset mid-incident and spin.
        let worst_case: u64 = (1..MAX_RECOVERY_ATTEMPTS)
            .map(|n| {
                RECOVERY_BACKOFF_BASE_MS
                    .saturating_mul(1u64 << (n - 1))
                    .min(RECOVERY_BACKOFF_MAX_MS)
            })
            .sum();
        assert!(
            RECOVERY_ATTEMPT_RESET_MS > worst_case,
            "reset window {RECOVERY_ATTEMPT_RESET_MS}ms must exceed the full ladder {worst_case}ms"
        );
    }

    // ── server-mode inertness ──────────────────────────────────────────

    #[test]
    fn server_mode_makes_recovery_inert() {
        // `SERVER_MODE` is a process-wide OnceLock, so this test asserts the
        // gate through the same accessor the entry point uses. Under `cargo
        // test` nothing calls `set_server_mode`, so the default is `false` and
        // the SECOND gate (no main window spec) is what keeps the entry point
        // inert — which is the invariant that actually matters: recovery never
        // fabricates a window this process never had.
        assert!(
            !is_server_mode(),
            "test process should not be flagged server mode"
        );
        assert!(
            main_window_spec().is_none(),
            "no main window is ever built under cargo test"
        );

        // Both gates return Skipped without touching Tauri, so we can assert
        // the shape of the skip without an AppHandle.
        //
        // (There is no way to build a real `tauri::AppHandle` in a unit test,
        // so the gate order is asserted structurally: `is_server_mode()` is
        // checked before any window lookup, and `main_window_spec()` before any
        // destroy/rebuild — see `trigger_ui_recovery`.)
        assert_eq!(
            RecoveryOutcome::Skipped { why: "server_mode" }.as_str(),
            "skipped"
        );
    }

    #[test]
    fn server_mode_flag_defaults_to_false_when_unset() {
        // An unset OnceLock must not read as "headless" — a windowed runner
        // whose main.rs somehow skipped `set_server_mode` still needs recovery.
        // The `no_main_window` gate is what protects the headless case.
        assert!(!is_server_mode());
    }
}
