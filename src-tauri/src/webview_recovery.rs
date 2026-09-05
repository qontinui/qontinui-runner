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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::{debug, error, info, warn};

use crate::window_placement::WindowPlacement;
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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

/// How long a freshly recreated window has to prove it is running a UI, by
/// ponging (plan `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure`
/// Phase 2).
///
/// **Recreate-scoped, and deliberately so.** The predicate is "a pong stamped
/// strictly AFTER the recreate finished" ([`classify_recreate_pong`]) — never a
/// relaxation of the global `last_pong > 0` guard in
/// [`crate::ui_error::ui_stale`]. That guard is what keeps a headless
/// server-mode runner (which never mounts a webview at all) and every runner's
/// boot window from reading as dead, and
/// `ui_stale_never_seen_is_not_stale_headless_server_mode_guard` pins it.
///
/// The calibration is borrowed rather than invented:
/// [`crate::ui_error::UI_STALE_AFTER_MS`] is already this codebase's answer to
/// "a live UI has checked in within this long", and Rust emits `ui-bridge-ping`
/// unconditionally every 3s, so this is ten consecutive missed pings.
pub const RECREATE_PONG_DEADLINE_MS: u64 = crate::ui_error::UI_STALE_AFTER_MS;

/// Poll interval while watching for that pong.
const RECREATE_PONG_POLL_MS: u64 = 250;

/// How long the single-flight latch may be held before the run holding it is
/// reported **wedged** rather than merely overlapping.
///
/// # Why this exists
///
/// [`recreate_main_window`]'s post-build probe blocks with no timeout by
/// design (see the long comment there, and [`verify_window_has_a_webview`]).
/// If the tao event loop is *independently* wedged the probe never returns,
/// [`InProgressGuard`] never drops, and every later trigger used to answer
/// `Skipped { why: "already_in_progress" }`, `attempts: 1`, `exhausted: false`
/// — byte-identical to a healthy 200 ms overlap. Recovery latched OFF
/// silently; on 2026-08-06 that silence cost two hours of blind diagnosis.
///
/// This constant makes the latched state **legible**, and nothing more.
/// Nothing steals the latch, nothing times out the `.await`, and there is no
/// `force` parameter into a second recreate — all three are the same rejected
/// hardening named in [`recreate_main_window`]'s comment (they would race a
/// second `destroy()` + `build()` against a label the first blocking thread is
/// still inside). The escape hatch for a genuinely wedged loop is the
/// separately-shipped force-close door.
///
/// # The derivation — no magic number
///
/// Every **bounded** cost one run can pay, plus one allowance for the single
/// cost that is deliberately unbounded:
///
/// * [`RECOVERY_BACKOFF_MAX_MS`] — the longest a single run sleeps in
///   [`GuardDecision::Backoff`] before it acts.
/// * [`WINDOW_LABEL_RELEASE_TIMEOUT_MS`] — the bounded label-release poll.
/// * [`RECREATE_PONG_DEADLINE_MS`] — the bounded post-recreate pong watch.
/// * a second [`RECOVERY_BACKOFF_MAX_MS`] as the allowance for the *unbounded*
///   `build_main_window` probe: a cold WebView2 profile on a loaded box is slow
///   but healthy, so this bound has to be generous rather than tight — the
///   false-positive class [`verify_window_has_a_webview`] refuses to create.
///
/// `recovery_wedge_threshold_cannot_drift_from_the_ladder` pins both ends: it
/// must exceed every bounded cost above, and it must stay strictly under
/// [`RECOVERY_ATTEMPT_RESET_MS`] — otherwise the loop guard would declare a
/// fresh incident before the wedge it is sitting inside was ever reported.
pub const RECOVERY_WEDGE_AFTER_MS: u64 = RECOVERY_BACKOFF_MAX_MS
    + WINDOW_LABEL_RELEASE_TIMEOUT_MS
    + RECREATE_PONG_DEADLINE_MS
    + RECOVERY_BACKOFF_MAX_MS;

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
pub(crate) const MAIN_WINDOW_BROWSER_ARGS: &str =
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
///
/// # The non-main-builder contract
///
/// "Both paths" means the two **main-window** paths only — it never covered
/// the runner's other three `WebviewWindowBuilder` sites
/// (`commands::terminal_windows::build_pop_out_webview`,
/// `click_overlay::initialize_overlay`,
/// `commands::project_preview::open_project_preview`), and that omission is
/// what caused plan `2026-08-10-popout-webview2-creation-failure`: pop-outs on
/// a secondary runner got no webview at all.
///
/// So, concretely, when you add a builder option here, decide which of the two
/// it is:
///
/// * **A WebView2 *environment* option** (`data_directory`,
///   `additional_browser_args` — anything that configures the WebView2
///   environment rather than the window) → put it in [`WebviewEnvOptions`] /
///   [`webview_env_options`], **not** in this chain. All four sites read that
///   one source, so every window gets it automatically.
/// * **A main-window-only option** (title, placement, min size, …) → it
///   belongs in this chain, and the other three sites deliberately do not get
///   it.
///
/// Adding an environment option directly to this chain re-opens that bug.
///
/// # Failure, and why it is fatal here
///
/// Returns `Err` when the builder fails **or** when
/// [`verify_window_has_a_webview`] cannot prove the window got a webview.
/// Both callers already treat that as fatal for their own scope, and both are
/// right to: `main.rs`'s setup closure aborts startup (a runner whose main
/// window has no webview can never serve any UI, and the pre-existing arm
/// already aborted on a builder error — this is the same failure class
/// arriving through a different door), and [`recreate_main_window`] turns it
/// into `RecoveryOutcome::Failed` so the ladder escalates instead of reporting
/// a successful rebuild of a hollow window.
///
/// ⚠ **Do not run this on a tokio worker** — see
/// [`verify_window_has_a_webview`]'s threading note. The two callers are the
/// setup closure (main thread, inline dispatch) and a `spawn_blocking` task.
pub fn build_main_window(
    app: &tauri::AppHandle,
    spec: &MainWindowSpec,
) -> Result<tauri::WebviewWindow, String> {
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
        .on_web_resource_request(crate::asset_headers::stamp_no_store_on_index);

    // The WebView2 environment options, from the same selector the three
    // non-main builders read — so "identical environment" is structural rather
    // than a comment two modules apart. Note this reads the spec being built
    // FROM, not `main_window_spec()`: at startup nothing is recorded yet.
    builder = apply_env_options(builder, webview_env_options(Some(spec)));

    // Inject the intended API port as a global so the frontend's synchronous
    // port-resolution fast-path (`window.__QONTINUI_PORT__`) resolves to the
    // *actual* runner port on temp/secondary instances instead of silently
    // falling through to the hardcoded 9876. Without this, hooks on a temp
    // runner route their reads at the primary.
    let intended_api_port = crate::mcp::types::get_mcp_api_port();
    builder =
        builder.initialization_script(format!("window.__QONTINUI_PORT__ = {};", intended_api_port));

    builder = spec.placement.configure_builder(builder);

    let win = builder
        .build()
        .map_err(|e| format!("WebviewWindowBuilder::build() for '{label}': {e}"))?;

    // `build()` returning `Ok` is NOT evidence of a webview. On the main window
    // that distinction is the difference between a runner with a UI and one
    // that only *looks* like it has one — and, on the recreate rung, between a
    // recovery ladder that escalates and one that stops on a hollow window.
    verify_window_has_a_webview(&win, label)?;

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

// ────────── WebView2 environment options for non-main webviews ───────────

/// The WebView2 environment options a webview is built with.
///
/// Extracted as a plain value so the *selection* can be asserted in a unit
/// test with no Tauri app and no window — the builder itself cannot be
/// inspected. See [`webview_env_options`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WebviewEnvOptions {
    /// WebView2 user-data folder. `None` means "apply nothing", which on
    /// Windows lets Tauri force `%LOCALAPPDATA%\<identifier>` instead.
    pub data_dir: Option<std::path::PathBuf>,
    /// `additional_browser_args`, or `None` to leave wry's defaults alone.
    pub browser_args: Option<&'static str>,
}

/// Which WebView2 environment options a webview must be built with, given the
/// spec the main window was (or will be) built from.
///
/// **This is the single option source for all four `WebviewWindowBuilder`
/// sites in the runner** — the main window here, plus the `term-N` pop-outs,
/// the click overlay and the project preview, which reach it through
/// [`apply_main_window_env_options`].
///
/// # Why both options, not just the folder
///
/// Plan `2026-08-10-popout-webview2-creation-failure` D2. Propagating the
/// folder alone would leave the two environments sharing **one** user-data
/// folder while still passing **different** `additionalBrowserArguments` —
/// the one configuration WebView2 is documented inconsistently on across
/// runtime versions. It would also leave every non-main webview without the
/// anti-throttling flags, and a pop-out terminal is the surface most likely to
/// sit backgrounded on another virtual desktop, which is exactly what
/// [`MAIN_WINDOW_BROWSER_ARGS`] exists to survive.
///
/// # `None`
///
/// `MAIN_WINDOW_SPEC` is a `OnceLock` written only by [`build_main_window`],
/// so `None` means "no main window was ever constructed" (server mode) — never
/// "not yet". There is nothing to mirror: apply nothing, and do not panic. The
/// non-main builder paths are unreachable under server mode, but the fix must
/// survive being reached.
pub(crate) fn webview_env_options(spec: Option<&MainWindowSpec>) -> WebviewEnvOptions {
    match spec {
        Some(spec) => WebviewEnvOptions {
            // `None` off-Windows and for the default profile
            // (`instance::webview2_data_dir` returns `None` on non-Windows),
            // which makes the whole propagation a no-op there.
            data_dir: spec.data_dir.clone(),
            browser_args: Some(MAIN_WINDOW_BROWSER_ARGS),
        },
        None => WebviewEnvOptions::default(),
    }
}

/// Apply an already-selected [`WebviewEnvOptions`] to a builder.
fn apply_env_options<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    mut builder: tauri::WebviewWindowBuilder<'a, R, M>,
    opts: WebviewEnvOptions,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    if let Some(args) = opts.browser_args {
        builder = builder.additional_browser_args(args);
    }
    if let Some(dir) = opts.data_dir {
        builder = builder.data_directory(dir);
    }
    builder
}

/// Give a **non-main** webview the same WebView2 environment as the live main
/// window. Every `WebviewWindowBuilder` outside [`build_main_window`] must
/// call this.
///
/// Without it, Tauri forces `%LOCALAPPDATA%\<identifier>` on any webview built
/// with no `data_directory` (`tauri` 2.11.1 `src/manager/webview.rs`, "in
/// `windows`, we need to force a data_directory but we do respect
/// user-specification"). On a secondary runner that is the **primary's**
/// profile root rather than the secondary's isolated folder, and WebView2
/// refuses it with `HRESULT(0x8007139F)` — a pop-out window with no webview at
/// all. Plan `2026-08-10-popout-webview2-creation-failure`.
pub(crate) fn apply_main_window_env_options<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'a, R, M>,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    apply_env_options(builder, webview_env_options(main_window_spec()))
}

// ────────────── post-build proof that a webview actually exists ──────────────

/// Prove that `window` actually got a webview, by asking the windowing backend
/// something only a live window can answer.
///
/// **Every `WebviewWindowBuilder` site in this crate calls this** — the main
/// window in [`build_main_window`], plus the `term-N` pop-outs, the click
/// overlay and the project preview. `webview_builders_all_apply_the_shared_env_options`
/// pins that as a source-level invariant rather than a convention.
///
/// # Why a window getter — and why NOT the two obvious checks
///
/// `WebviewWindowBuilder::build()` reports success for a window that has no
/// webview at all. `WryWindowDispatcher::create_window` (tauri-runtime-wry
/// 2.11.2 `src/lib.rs:~300-345`) *sends* `Message::CreateWindow` to the event
/// loop and returns `Ok(DetachedWindow)` immediately — construction has not
/// been attempted when `build()` returns. The event loop's handler for that
/// message (`src/lib.rs:4084-4091`) is `Ok(w) => windows.insert(…)` /
/// `Err(e) => log::error!("{e}")`: on failure it logs and **never inserts the
/// window into wry's `windows` map**. That `log::error!` *is* the
/// `ERROR tauri_runtime_wry: failed to create webview: …` line in the runner
/// logs. Tauri, meanwhile, inserted the window into its OWN registry
/// unconditionally (`tauri` 2.11.1 `src/manager/webview.rs:610-632`,
/// `attach_webview`).
///
/// So two checks that look obvious both fail **silently open**. Do not
/// reinstate them:
///
/// * *"the label is present in `app.webview_windows()`"* — **always true**.
///   That map is Tauri's own; a hollow window is in it exactly like a healthy
///   one.
/// * *"it answers a trivial `eval`"* — **fire-and-forget**. On the default
///   (non-`tracing`-feature) arm `WebviewMessage::EvaluateScript` carries no
///   reply channel (tauri-runtime-wry 2.11.2 `src/lib.rs:3777-3782`), so
///   `Webview::eval` returns `Ok(())` whether or not anything ran.
///
/// A **window getter** is the direct falsifier of the mechanism above. The
/// getter macro (`src/lib.rs:196-211`) sends `Message::Window(window_id, …)`
/// with a reply `tx` and maps a closed channel to
/// `Error::FailedToReceiveMessage`; the handler (`src/lib.rs:3372-3381`)
/// early-returns when the id is **absent from wry's `windows` map** — precisely
/// the state a failed `Message::CreateWindow` leaves behind — dropping `tx`
/// unsent. So it returns `Err` on exactly this failure and `Ok` on a healthy
/// window. `is_visible()` is used here; `inner_size()` / `scale_factor()` /
/// any other `Message::Window` getter would do. Only the `Ok`/`Err` is
/// meaningful — never the `bool` (the click overlay is built `visible(false)`
/// on purpose, and a pop-out is `show()`n by its caller afterwards).
///
/// # Properties, stated so nobody "hardens" them away
///
/// * **Ordered, not racy.** `build()` and this probe go through the same
///   serialized event-loop queue (`send_user_message`, `src/lib.rs:235-243`,
///   which runs inline when called on the main thread), so
///   `Message::CreateWindow` is always handled before this `Message::Window`.
///   **No sleep, no retry loop, no polling** — adding one would be
///   cargo-culting.
/// * **Nested message pumping does not reopen that race, though it looks like
///   it should.** wry builds the WebView2 environment under
///   `webview2_com::wait_with_pump` (webview2-com 0.38.2 `lib.rs:60-81`) — a
///   nested `GetMessageA`/`DispatchMessageA` pump that runs *inside* the
///   `Message::CreateWindow` handler and *before* `windows.insert`. A probe
///   message pumped there would be dispatched while the window is still
///   absent from the map, i.e. every healthy build would report `Err`. It does
///   not happen: tao 0.35.0 `EventLoopRunner::send_event`
///   (`platform_impl/windows/event_loop/runner.rs:208-226`) checks
///   `should_buffer()` (`:143-148`, true for as long as `event_handler` is
///   taken — which it is, throughout the outer handler) and pushes the event
///   onto `event_buffer` instead of dispatching it. The buffer is flushed by
///   `dispatch_buffered_events()` (`:259-271`) only after the outer handler
///   returns, i.e. after the insert. **Do not "fix" this with a delay.**
/// * **Platform-neutral.** No `#[cfg(windows)]`, no WebView2 types, no HRESULT
///   matching. Off-Windows it is a cheap always-`Ok` assertion.
/// * **It covers the never-created case**, which
///   [`attach_non_main_process_failed_handler`] structurally cannot see — that
///   handler is a subscription on a webview that exists.
///
/// # Threading — this call blocks, and deliberately has no timeout
///
/// Off the main thread the getter takes the proxy branch and `rx.recv()`s with
/// **no timeout** (tauri-runtime-wry 2.11.2 `src/lib.rs:196-211`), so it blocks
/// its thread until the event loop answers. Callers must therefore run it on
/// the **main thread** (where `send_user_message` dispatches inline) or on a
/// **blocking** thread (`tauri::async_runtime::spawn_blocking`) — never
/// directly on a tokio worker, where a cold-profile WebView2 environment
/// creation (seconds) or a wedged event loop (unbounded) would starve the async
/// runtime.
///
/// A short timeout was considered and **rejected**: a cold-profile build on a
/// loaded box is slow but healthy, so a bound tight enough to be useful would
/// turn healthy pop-outs into reported failures — the same false-positive class
/// this probe exists to avoid producing. A wedged event loop costs one blocking
/// thread instead, which is recoverable and cannot lie.
///
/// Plan `2026-08-10-popout-webview2-creation-failure` Phase 3 / D4.
pub(crate) fn verify_window_has_a_webview(
    window: &tauri::WebviewWindow,
    label: &str,
) -> Result<(), String> {
    window
        .is_visible()
        .map(|_| ())
        .map_err(|e| no_webview_error(label, &e.to_string()))
}

/// The message a failed [`verify_window_has_a_webview`] produces.
///
/// Split out so it can be asserted without a Tauri app, and so every call site
/// words the failure identically. It must name the **real** cause rather than
/// the getter that surfaced it: a getter that cannot answer looks like a
/// stalled window, but what actually happened is that no webview was ever
/// created, and the log line proving it is wry's own
/// `failed to create webview`.
pub(crate) fn no_webview_error(label: &str, backend_error: &str) -> String {
    format!(
        "Window {} was built but has no webview — the windowing backend does not know this \
         window ({}). No webview was created; check the log for `failed to create webview`.",
        label, backend_error
    )
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

/// Why recovery was asked for. Callers by design:
///
/// * [`RecoveryReason::ProcessFailed`] — the Phase 1a push event (this module).
/// * [`RecoveryReason::HeartbeatStale`] — the Phase 1b staleness backstop,
///   wired by the coordinator during integration. **Not wired here**; this
///   variant exists so the signature is already right for it.
/// * [`RecoveryReason::Manual`] — the operator/debug HTTP route.
/// * [`RecoveryReason::NativeUiThreadHung`] — the native message-loop probe
///   (`health_monitor::ui_thread_pumping`), plan
///   `2026-08-19-runner-blocked-ui-thread-cannot-be-closed` Phase 4. **Detect
///   and surface only**: see [`plan_action`] for why no action can help, and
///   [`report_native_ui_thread_hang`] for the surface it does use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    ProcessFailed(ProcessFailureKind),
    HeartbeatStale,
    Manual,
    NativeUiThreadHung,
}

impl RecoveryReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProcessFailed(_) => "process_failed",
            Self::HeartbeatStale => "heartbeat_stale",
            Self::Manual => "manual",
            Self::NativeUiThreadHung => "native_ui_thread_hung",
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
/// Escalation happens **across attempts**, not inside one call — and the two
/// rungs are verified differently, which this doc used to flatten into "this
/// module deliberately does not read `ui_bridge_last_pong`". That is no longer
/// true of the recreate rung:
///
/// * **Reload is still unverified.** Nothing here can observe whether
///   `location.reload()` took, so the cheap rung is tried once and any
///   subsequent request for the same incident escalates. A failed `eval` is
///   hard evidence and escalates immediately inside the call — see
///   [`trigger_ui_recovery`].
/// * **Recreate IS verified**, since Phase 2 of plan
///   `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure`.
///   [`trigger_ui_recovery`] reads `ui_bridge_last_pong` after a successful
///   rebuild and requires a pong stamped **strictly after** it
///   ([`classify_recreate_pong`]), so a window that rebuilds blank reports
///   `Failed` and lets this ladder escalate instead of claiming success
///   forever. That read is **recreate-scoped**: it compares against the
///   recreate's own completion instant and never relaxes the global
///   `last_pong > 0` guard in [`crate::ui_error::ui_stale`].
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
        // The native message loop itself stopped pumping (2026-08-19). `None`
        // is not a gap in this ladder — it is a property of the failure, and
        // both rungs were checked against the source rather than assumed:
        //
        // * **Reload** is `window.eval("location.reload()")`
        //   ([`reload_main_webview`]), which dispatches through the very loop
        //   that is wedged.
        // * **Recreate** is `destroy()` + rebuild, and `destroy()` only
        //   *enqueues* onto that same loop; [`recreate_main_window`]'s
        //   label-release poll would then burn its full
        //   [`WINDOW_LABEL_RELEASE_TIMEOUT_MS`] and return `Err`.
        //
        // Detect and surface; never attempt a recovery that cannot run. A
        // force-exit is not on the table here either: the plan permits that
        // only downstream of an explicit user close action, never on bare hang
        // detection, because exiting destroys every in-flight session — 102 of
        // them in the originating incident.
        RecoveryReason::NativeUiThreadHung => RecoveryAction::None,
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

// ───────────────────── in-flight latches, with an age ────────────────────

/// Process-start `Instant`, so a monotonic timestamp fits in one `AtomicU64`.
fn monotonic_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Monotonic "now" for the latches, in ms since [`monotonic_epoch`].
///
/// Monotonic rather than wall-clock on purpose: an in-flight age must not move
/// because NTP stepped the clock or the machine slept, which is exactly the
/// arithmetic that would turn a healthy overlap into a reported wedge.
fn latch_now_ms() -> u64 {
    monotonic_epoch().elapsed().as_millis() as u64
}

/// A single-flight latch that also records **when** it was taken, so a reader
/// can tell a healthy 200 ms overlap from a run that has latched recovery OFF.
///
/// # One atomic, not two
///
/// A `bool` plus a separate timestamp cannot be taken together: a reader
/// landing between the swap and the timestamp store would see "held" next to
/// the *previous* run's stamp and report a wedge that never happened. So the
/// whole state is one `AtomicU64`: `0` means free, anything else is
/// `taken_at_ms + 1`. The `+1` bias is what frees `0` as the sentinel — a
/// process that takes the latch inside its first millisecond has a legitimate
/// `now_ms` of `0`.
///
/// # Reading only
///
/// The instant exists so `/health`, `POST /ui/recover` and the
/// `wedge-incidents.log` breadcrumb can compute an age. **Nothing here steals
/// the latch, ages it out, or hands anyone a way past it** — see
/// [`RECOVERY_WEDGE_AFTER_MS`] for why every such "hardening" is rejected.
///
/// # The clock is injected
///
/// `now_ms` is a parameter, exactly as [`LoopGuard::decide`] takes one. There
/// is no way to build a real `tauri::AppHandle` in a unit test (see
/// `server_mode_makes_recovery_inert`), so age arithmetic that read the clock
/// itself would be untestable out-of-line.
pub struct InFlightLatch {
    /// `0` = free; otherwise `taken_at_ms + 1` (see the bias note above).
    taken_at_ms: AtomicU64,
}

impl InFlightLatch {
    pub const fn new() -> Self {
        Self {
            taken_at_ms: AtomicU64::new(0),
        }
    }

    /// Take the latch if it is free. `Err(age_ms)` reports how long the
    /// current holder has held it.
    pub fn try_take(&self, now_ms: u64) -> Result<(), u64> {
        match self.taken_at_ms.compare_exchange(
            0,
            now_ms.saturating_add(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => Ok(()),
            Err(held) => Err(now_ms.saturating_sub(held - 1)),
        }
    }

    /// Take the latch unconditionally, replacing any existing stamp.
    ///
    /// For [`WINDOW_SWAP_LATCH`], which is **not** a mutual-exclusion device:
    /// it is the exit veto's "the window is genuinely gone right now" flag, and
    /// it is only ever set from inside [`RECOVERY_LATCH`]'s own critical
    /// section. Behaviour is what the plain `store(true)` did before the age
    /// was added — deliberately unchanged, since the exit remedy shipped
    /// separately.
    pub fn take_unconditional(&self, now_ms: u64) {
        self.taken_at_ms
            .store(now_ms.saturating_add(1), Ordering::SeqCst);
    }

    /// Release it. Idempotent, so a `Drop` guard can never double-free.
    pub fn release(&self) {
        self.taken_at_ms.store(0, Ordering::SeqCst);
    }

    pub fn is_held(&self) -> bool {
        self.taken_at_ms.load(Ordering::SeqCst) != 0
    }

    /// How long the current holder has held it, or `None` when free.
    pub fn in_flight_age_ms(&self, now_ms: u64) -> Option<u64> {
        match self.taken_at_ms.load(Ordering::SeqCst) {
            0 => None,
            held => Some(now_ms.saturating_sub(held - 1)),
        }
    }
}

impl Default for InFlightLatch {
    fn default() -> Self {
        Self::new()
    }
}

/// How a latch looks from outside — `/health`, `POST /ui/recover`, the log.
///
/// `wedged` is the whole point of the type: before it, a latched-off recovery
/// and a 200 ms overlap were the same bytes on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatchReport {
    /// Someone holds the latch right now.
    pub in_flight: bool,
    /// For how long, in ms. `None` when free — UNKNOWN is spelled `null`
    /// rather than `0`, which would read as "just started".
    pub in_flight_ms: Option<u64>,
    /// Held longer than [`RECOVERY_WEDGE_AFTER_MS`], i.e. longer than the whole
    /// ladder can legitimately take.
    pub wedged: bool,
}

/// Pure classifier, so the wedge threshold is assertable without a latch.
pub fn classify_latch(age_ms: Option<u64>) -> LatchReport {
    LatchReport {
        in_flight: age_ms.is_some(),
        in_flight_ms: age_ms,
        wedged: age_ms.is_some_and(|ms| ms >= RECOVERY_WEDGE_AFTER_MS),
    }
}

/// Single-flight for [`trigger_ui_recovery`], plus the age that makes a stuck
/// run reportable.
static RECOVERY_LATCH: InFlightLatch = InFlightLatch::new();

/// Held between `destroy()` and the rebuild — the exit veto's flag. Carries the
/// same age term so a permanent [`ExitVeto::VetoSwapInFlight`] is legible;
/// **what the veto decides is unchanged**.
static WINDOW_SWAP_LATCH: InFlightLatch = InFlightLatch::new();

/// Latches once the current wedge has been written to `wedge-incidents.log`.
///
/// The heartbeat backstop re-triggers recovery on EVERY stale tick
/// (`heartbeat.rs`), so without this a wedge would append a line per tick
/// forever. Cleared by [`InProgressGuard::drop`]: the next wedge is a new
/// incident and gets its own line.
static RECOVERY_WEDGE_REPORTED: AtomicBool = AtomicBool::new(false);

/// [`classify_latch`] over the live recovery latch.
pub fn recovery_latch_report() -> LatchReport {
    classify_latch(RECOVERY_LATCH.in_flight_age_ms(latch_now_ms()))
}

/// [`classify_latch`] over the live window-swap latch.
pub fn window_swap_report() -> LatchReport {
    classify_latch(WINDOW_SWAP_LATCH.in_flight_age_ms(latch_now_ms()))
}
/// Latches once the user has been told this incident is terminal, so repeated
/// `ProcessFailed` events cannot spam a dialog at someone whose UI is already
/// gone. Cleared by [`LoopGuard::decide`]'s incident reset, alongside the
/// attempt counter it belongs to.
static EXHAUSTION_SURFACED: AtomicBool = AtomicBool::new(false);

/// Same idea, one rung over: latches once the user has been told the native
/// message loop is hung.
///
/// A **separate** latch from [`EXHAUSTION_SURFACED`], deliberately. The two
/// incidents are independent (a dead WebView2 host and a blocked host thread
/// are different failures with different text), so neither may silence the
/// other. Cleared by [`clear_native_ui_thread_hang`] when the loop starts
/// pumping again, which is that rung's equivalent of the incident reset.
static NATIVE_HANG_SURFACED: AtomicBool = AtomicBool::new(false);

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
    WINDOW_SWAP_LATCH.is_held()
}

/// Releases [`WINDOW_SWAP_LATCH`] on every exit path, including a panic or a
/// dropped future — a stuck flag would make the runner un-exitable.
struct SwapGuard;

impl Drop for SwapGuard {
    fn drop(&mut self) {
        WINDOW_SWAP_LATCH.release();
    }
}

/// What to do with an observed `RunEvent::ExitRequested`.
///
/// # Why this is not just [`window_swap_in_progress`]
///
/// It used to be, and that is precisely how the runner killed itself at
/// 2026-08-06T01:00:56Z. [`WINDOW_SWAP_LATCH`] is held for the *duration of
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
///   "last window closed". That is the one case [`WINDOW_SWAP_LATCH`]
///   answers, and it is kept for exactly that case. It now also carries the
///   age at which the swap started, so a PERMANENT `VetoSwapInFlight` is
///   legible on `/health` and in the veto log line — **reporting only; what
///   this function decides is unchanged.**
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
    const TITLE: &str = "Qontinui Runner — the window stopped working";
    let body = format!(
        "The runner's UI host crashed and could not be restarted after {attempts} attempts.\n\n\
         Automation and the API on port 9876 are still running — your sessions are NOT lost.\n\n\
         To get the window back, restart the Qontinui Runner application. If it keeps \
         happening, updating the Microsoft Edge WebView2 Runtime is the fix, since the \
         crash originates inside WebView2 rather than in Qontinui."
    );

    if surface_incident_to_user(app, &EXHAUSTION_SURFACED, TITLE, &body) {
        error!(
            attempts,
            "UI recovery exhausted — surfaced to the user natively (notification + dialog)"
        );
    }
}

/// The **third rung**: the native message loop has stopped pumping.
///
/// Called by `health_monitor`'s `WedgeKind::UiThread` detector once the probe
/// has failed `WEDGE_FAILURE_THRESHOLD`-many consecutive samples, from that
/// module's dedicated OS thread. Plan
/// `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 4.
///
/// # Why this reports instead of recovering
///
/// [`plan_action`] maps [`RecoveryReason::NativeUiThreadHung`] to
/// [`RecoveryAction::None`], so [`trigger_ui_recovery`] would (correctly) skip
/// it as a no-op reason without telling anybody. Every rung of that ladder
/// dispatches through the loop that is wedged, so attempting one would burn a
/// timeout and change nothing — while spending attempt budget the
/// [`LoopGuard`] is holding for a *real* webview crash, which is why this path
/// deliberately does **not** consume it. What the user needs from this
/// condition is the truth, delivered on a channel the hang cannot block.
///
/// # Which channels actually survive the hang
///
/// Checked in the plugin sources rather than assumed, because it decides
/// whether this function does anything at all:
///
/// * **The breadcrumb** (`health_monitor`, `wedge-incidents.log`) always
///   works — a plain file append from the monitor's own OS thread. It is the
///   durable record, and the reason it matters is that `runner-lifecycle.log`
///   is truncated at every runner startup, so a restart destroys the evidence
///   of the wedge that provoked it.
/// * **The notification** works on Windows 8+: `tauri-plugin-notification`'s
///   `show()` goes to the OS toast API off the main thread.
/// * **The dialog** does **not** work during the hang:
///   `tauri-plugin-dialog`'s `show_message_dialog` wraps the whole call in
///   `AppHandle::run_on_main_thread`, i.e. an enqueue onto the blocked loop.
///   It is still dispatched (the enqueue is non-blocking and cannot make
///   things worse) and will appear if the loop resumes — but it must never be
///   counted on as *the* surface for this failure.
///
/// Best-effort by contract, like every other channel here.
pub fn report_native_ui_thread_hang(app: &tauri::AppHandle, unresponsive_for_secs: u64) {
    const TITLE: &str = "Qontinui Runner — the window has stopped responding";
    let body = format!(
        "The runner's window has not responded for {unresponsive_for_secs} seconds: its native \
         message loop is blocked, so the window will not repaint and clicking it — including \
         the X button — does nothing.\n\n\
         Automation and the API on port 9876 are still running, and your sessions are NOT \
         lost. The runner will not restart itself to clear this: that would destroy every \
         session currently in flight.\n\n\
         If the window does not come back on its own, end the Qontinui Runner process from \
         Task Manager. An incident line has been written to wedge-incidents.log in the \
         runner's dev-logs directory."
    );

    if surface_incident_to_user(app, &NATIVE_HANG_SURFACED, TITLE, &body) {
        error!(
            unresponsive_for_secs,
            "Native UI thread hang surfaced to the user (notification always; dialog only if \
             the loop resumes)"
        );
    }
}

/// Re-arm [`report_native_ui_thread_hang`] once the loop is pumping again.
///
/// Called from `health_monitor`'s recovery edge. Without it the first hang of
/// a process's life would be the only one the user ever hears about.
pub fn clear_native_ui_thread_hang() {
    NATIVE_HANG_SURFACED.store(false, Ordering::SeqCst);
}

/// The one place an incident becomes an OS-native notification + dialog.
///
/// Returns `true` when this call is the one that surfaced it, so the caller
/// can log exactly once. `latch` makes that at-most-once per incident:
/// repeated detections must not stack dialogs on someone who already cannot
/// use the app.
fn surface_incident_to_user(
    app: &tauri::AppHandle,
    latch: &AtomicBool,
    title: &str,
    body: &str,
) -> bool {
    // Server mode has no desktop to surface to. `trigger_ui_recovery` returns
    // before reaching here, but this is defence in depth for any future caller.
    if is_server_mode() {
        return false;
    }
    if latch.swap(true, Ordering::SeqCst) {
        debug!(title, "UI incident already surfaced for this incident");
        return false;
    }

    {
        use tauri_plugin_notification::NotificationExt;
        if let Err(e) = app.notification().builder().title(title).body(body).show() {
            warn!(error = %e, "UI incident: could not post the notification");
        }
    }

    {
        use tauri_plugin_dialog::DialogExt;
        // Non-blocking `show`: a modal `blocking_show` here would park a
        // runtime thread on user input during an active incident.
        app.dialog()
            .message(body)
            .title(title)
            .kind(tauri_plugin_dialog::MessageDialogKind::Error)
            .show(|_| {});
    }

    true
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
    /// The single-flight latch has been held longer than the whole ladder can
    /// take ([`RECOVERY_WEDGE_AFTER_MS`]): recovery is **wedged**, not merely
    /// overlapping, and is latched OFF until the run holding it returns.
    ///
    /// Distinct from `Skipped { why: "already_in_progress" }` on purpose. Those
    /// two were byte-identical on every surface until 2026-08-06 — same
    /// `skipped`, same `attempts: 1`, same `exhausted: false` — and that
    /// silence is the defect this variant exists to end. It is a **report**,
    /// not a lever: the caller still must not retry, and nothing anywhere
    /// steals the latch.
    #[serde(rename = "recovery_wedged")]
    Wedged { in_flight_ms: u64 },
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
            Self::Wedged { .. } => "recovery_wedged",
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
    //
    //    The latch records WHEN it was taken, so a run that never finishes is
    //    reported as `Wedged` instead of being indistinguishable from a healthy
    //    overlap. Nothing here steals it, times it out, or offers a `force`
    //    past it — see `RECOVERY_WEDGE_AFTER_MS`.
    if let Err(in_flight_ms) = RECOVERY_LATCH.try_take(latch_now_ms()) {
        if in_flight_ms >= RECOVERY_WEDGE_AFTER_MS {
            report_recovery_wedge(reason, in_flight_ms);
            return RecoveryOutcome::Wedged { in_flight_ms };
        }
        debug!(
            reason = reason.as_str(),
            in_flight_ms, "UI recovery skipped: a recovery run is already in flight"
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
            // Phase 2 (plan
            // `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure`).
            // `Ok` here means the window was rebuilt and HAS a webview — it
            // does NOT mean a UI is running inside it. Require a pong stamped
            // strictly after this instant, so a rebuild that comes up blank
            // reports `Failed` and lets the loop guard escalate on the next
            // trigger, instead of claiming `Recreated` over a dead window.
            let recreate_done_ms = now_ms();
            match verify_recreate_took(app, recreate_done_ms).await {
                RecreatePongVerdict::Live => {
                    info!("UI recovery: main window recreated and the rebuilt UI has ponged");
                    RecoveryOutcome::Recreated
                }
                RecreatePongVerdict::Unverifiable => {
                    // UNKNOWN is not failure: with no managed `AppState` there
                    // is no pong stamp to read, and inventing a verdict from
                    // that absence would fail every healthy recreate.
                    warn!(
                        "UI recovery: main window recreated, but the pong stamp is not \
                         readable in this process — recreate reported as done (UNKNOWN, \
                         deliberately not a failure)"
                    );
                    RecoveryOutcome::Recreated
                }
                // The watch loop resolves `Waiting` itself; it only ever
                // returns a settled verdict. Matched rather than assumed.
                RecreatePongVerdict::NoPong | RecreatePongVerdict::Waiting => {
                    let detail = format!(
                        "main window rebuilt, but no UI-Bridge pong arrived within \
                         {RECREATE_PONG_DEADLINE_MS}ms of the recreate — the rebuilt window \
                         has no live UI"
                    );
                    error!(detail = %detail, "UI recovery: the recreate produced no live UI");
                    RecoveryOutcome::Failed { detail }
                }
            }
        }
        Err(e) => {
            error!(error = %e, "UI recovery: main window recreate FAILED");
            RecoveryOutcome::Failed { detail: e }
        }
    }
}

/// Surface a latched-off recovery: one `error!` and one durable line in
/// `wedge-incidents.log`, at most once per wedge.
///
/// The breadcrumb goes into the **existing** incident sink rather than a new
/// file. `wedge-incidents.log` is already the one place to read after an
/// unexplained outage — and `runner-lifecycle.log` is truncated at every
/// startup, so a restart destroys the evidence of the wedge that provoked it.
/// Same writer, same grammar as `ui_thread_wedged` / `backend_wedged`.
fn report_recovery_wedge(refused: RecoveryReason, in_flight_ms: u64) {
    if RECOVERY_WEDGE_REPORTED.swap(true, Ordering::SeqCst) {
        debug!(
            refused_reason = refused.as_str(),
            in_flight_ms, "UI recovery still wedged (already reported for this incident)"
        );
        return;
    }
    error!(
        refused_reason = refused.as_str(),
        in_flight_ms,
        wedge_after_ms = RECOVERY_WEDGE_AFTER_MS,
        "UI recovery WEDGED — the run in flight has held the single-flight latch longer than \
         the whole ladder can take. Recovery is latched OFF until it returns, so the window \
         will not be rebuilt; this is reported rather than broken open, because stealing the \
         latch would race a second destroy()+build() against the same label."
    );
    crate::health_monitor::append_wedge_incident(
        "recovery_wedged",
        &format!(
            "webview recovery wedged — the single-flight latch has been held for \
             {in_flight_ms}ms (> {RECOVERY_WEDGE_AFTER_MS}ms, the ladder's own maximum). \
             Recovery is latched OFF: every later trigger is refused until the run in \
             flight returns. The trigger refused when this line was written was \
             {}.",
            refused.as_str()
        ),
    );
}

/// Verdict of the post-recreate pong watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecreatePongVerdict {
    /// A pong stamped strictly after the recreate landed — the rebuilt webview
    /// is demonstrably running a UI.
    Live,
    /// No qualifying pong yet, and the deadline has not passed: keep waiting.
    /// Only [`classify_recreate_pong`] returns this; the watch loop resolves it.
    Waiting,
    /// The deadline passed with no pong after the recreate.
    NoPong,
    /// The pong stamp could not be read in this process. UNKNOWN — never a
    /// failure. Only the watch loop returns this.
    Unverifiable,
}

/// Did the recreate produce a live UI? Pure, so it is testable without an
/// `AppHandle` (there is no way to build one in a unit test — see
/// `server_mode_makes_recovery_inert`).
///
/// # Recreate-scoped, and why that matters
///
/// The predicate is `last_pong_ms > recreate_done_ms` — **strictly** after. A
/// pong from before the `destroy()` proves nothing about the window that
/// replaced it, and a `>=` would let one land on the same millisecond boundary.
///
/// This is deliberately NOT a relaxation of the global `last_pong > 0` guard in
/// [`crate::ui_error::ui_stale`]. That guard is what keeps a headless
/// server-mode runner — which never mounts a webview at all — and every
/// runner's boot window from reading as dead, and
/// `ui_stale_never_seen_is_not_stale_headless_server_mode_guard` pins it. Here,
/// `last_pong_ms == 0` simply fails the strict comparison like any other stamp
/// older than the recreate: it waits, and then reports `NoPong`. That is the
/// honest answer in this scope only, because reaching it means a window was
/// just rebuilt in a process that is not in server mode (hard gate 1 of
/// [`trigger_ui_recovery`]) and that had a recorded [`MainWindowSpec`]
/// (hard gate 2).
///
/// `elapsed_ms` is measured on the MONOTONIC clock ([`latch_now_ms`]), not from
/// two wall-clock reads: an NTP step backwards during the watch would otherwise
/// saturate the age to 0 and wait forever.
pub fn classify_recreate_pong(
    last_pong_ms: u64,
    recreate_done_ms: u64,
    elapsed_ms: u64,
    deadline_ms: u64,
) -> RecreatePongVerdict {
    if last_pong_ms > recreate_done_ms {
        return RecreatePongVerdict::Live;
    }
    if elapsed_ms >= deadline_ms {
        return RecreatePongVerdict::NoPong;
    }
    RecreatePongVerdict::Waiting
}

/// Watch `ui_bridge_last_pong` for [`RECREATE_PONG_DEADLINE_MS`] and settle
/// [`classify_recreate_pong`].
///
/// Runs inside the recovery latch, which is accounted for: the bounded cost of
/// this wait is one of the four terms in [`RECOVERY_WEDGE_AFTER_MS`].
async fn verify_recreate_took(
    app: &tauri::AppHandle,
    recreate_done_ms: u64,
) -> RecreatePongVerdict {
    let Some(last_pong) = ui_bridge_last_pong(app) else {
        return RecreatePongVerdict::Unverifiable;
    };
    let started_ms = latch_now_ms();
    loop {
        match classify_recreate_pong(
            last_pong.load(Ordering::Relaxed),
            recreate_done_ms,
            latch_now_ms().saturating_sub(started_ms),
            RECREATE_PONG_DEADLINE_MS,
        ) {
            RecreatePongVerdict::Waiting => {
                tokio::time::sleep(std::time::Duration::from_millis(RECREATE_PONG_POLL_MS)).await;
            }
            settled => return settled,
        }
    }
}

/// The `ui_bridge_last_pong` stamp, or `None` when this process has no managed
/// `AppState` (a test rig, or a startup that never got that far).
///
/// `try_state` rather than `state`, which panics on an unmanaged type — a panic
/// here would turn "we could not verify" into a second failure during an
/// incident.
fn ui_bridge_last_pong(app: &tauri::AppHandle) -> Option<std::sync::Arc<AtomicU64>> {
    use tauri::Manager;
    app.try_state::<std::sync::Arc<crate::commands::AppState>>()
        .map(|s| s.ui_bridge_last_pong.clone())
}

/// Releases [`RECOVERY_LATCH`] even if the recovery future is dropped.
///
/// Also re-arms [`RECOVERY_WEDGE_REPORTED`], so a future wedge is a fresh
/// incident with its own breadcrumb rather than a silent repeat.
struct InProgressGuard;

impl Drop for InProgressGuard {
    fn drop(&mut self) {
        RECOVERY_LATCH.release();
        RECOVERY_WEDGE_REPORTED.store(false, Ordering::SeqCst);
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
    WINDOW_SWAP_LATCH.take_unconditional(latch_now_ms());
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

    // The cached main-window HWND now names a destroyed window. Forget it, or
    // the native-hang probe (`health_monitor::ui_thread_pumping`) keeps
    // `SendMessageTimeoutW`-ing a dead handle, reports UNKNOWN — which is
    // deliberately never escalated — and native-hang detection is off for the
    // rest of this process's life. `main_hwnd()` also self-heals via `IsWindow`;
    // this is the explicit door, at the one site that knows the window is gone.
    crate::ui_thread_probe::invalidate_main_hwnd();

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

    // Why `spawn_blocking` — corrected 2026-08-19. The reason this comment used
    // to give ("`build()` dispatches to the event loop and blocks the calling
    // thread until it answers") is **false**: off the main thread
    // `WryWindowDispatcher::create_window` only *sends* `Message::CreateWindow`
    // and returns `Ok` immediately (tauri-runtime-wry 2.11.2
    // `src/lib.rs:~300-345`) — that fire-and-forget is the whole reason
    // `build_main_window` has to probe afterwards at all.
    //
    // The real reason is that probe: `verify_window_has_a_webview` is a
    // `Message::Window` getter whose `rx.recv()` has **no timeout**
    // (`src/lib.rs:196-211`), so on a non-main thread it blocks until the event
    // loop answers — seconds while WebView2 builds a cold profile under
    // `webview2_com::wait_with_pump`, unbounded if the loop is wedged. That
    // must cost a blocking thread, never a tokio worker.
    //
    // UNVERIFIED (needs the coordinator's live kill test on a temp runner):
    // whether a WebView2 user-data directory (`spec.data_dir`, set for
    // temp/secondary runners) is still locked by the crashed browser process's
    // orphaned siblings at this point. If it is, `build()` returns an error
    // here — which the loop guard handles by escalating and ultimately
    // exhausting rather than spinning. Everything else about this path is
    // established from the Tauri/wry sources; this one is not statically
    // decidable.
    //
    // ⚠ **This probe runs inside the `RECOVERY_LATCH` critical section**, and
    // that interaction is a real cost of the no-timeout decision rather than an
    // oversight. The latch is taken by the single-flight `try_take` in
    // `trigger_ui_recovery` and released only by `InProgressGuard::drop`. If
    // the tao event loop is itself wedged, the probe folded into
    // `build_main_window` never returns, this `.await` never resumes, the
    // guard never drops, and every later recovery attempt is refused —
    // recovery latches OFF. Three things bound it, and none of them is
    // "unlikely":
    //
    // * It needs an **independently** wedged loop. The failure this ladder
    //   exists for — a WebView2 browser-process death — leaves tao running, so
    //   the getter answers and the guard drops normally.
    // * The obvious hardening is rejected. A timeout on *this* `await` would
    //   let a second recovery run `destroy()` + `build()` against the same
    //   label while the first blocking thread is still inside
    //   `build_main_window` — exactly the concurrent-recreate race the
    //   single-flight exists to prevent, and a `WindowLabelAlreadyExists`
    //   machine. A timeout on the **probe** is rejected separately and for a
    //   different reason: see `verify_window_has_a_webview`, where a
    //   cold-profile build is slow but healthy.
    // * A wedged tao loop is not a state this ladder could recover from even
    //   with a free latch — the recreate it would unblock dispatches through
    //   that same loop.
    //
    // What changed on 2026-09-04 (plan
    // `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure` Phase 1) is
    // ONLY the silence, not any of the three rejections above. Until then the
    // latched-off state answered `Skipped { why: "already_in_progress" }`,
    // `attempts: 1`, `exhausted: false` — byte-identical to a healthy 200 ms
    // overlap on every surface, which cost two hours of blind diagnosis on
    // 2026-08-06. `RECOVERY_LATCH` now records WHEN it was taken, so a refusal
    // past `RECOVERY_WEDGE_AFTER_MS` reports `RecoveryOutcome::Wedged` with the
    // age, on `/health`, on `POST /ui/recover` and in `wedge-incidents.log`.
    // Nothing steals the latch, nothing times out this `.await`, and there is
    // no `force` past the single flight; the escape hatch for a wedged loop
    // remains the separately-shipped force-close door.
    let app_for_build = app.clone();
    let built = spawn_blocking_tracked(move || build_main_window(&app_for_build, &spec))
        .await
        .map_err(|e| format!("recreate task panicked: {e}"))?;

    // `build_main_window` already folds its post-build webview probe into this
    // `Err`, so reaching the `Ok` arm means the rebuilt window HAS a webview —
    // the terminal rung of the ladder can no longer report success over a
    // hollow main window.
    let win = built?;

    // Re-arm detection on the fresh webview — otherwise the first recovery
    // would be the last one this process could ever notice.
    attach_process_failed_handler(&win);

    // ── Re-cache the main-window HWND ──
    //
    // `invalidate_main_hwnd()` above emptied the memo and NOTHING refilled it,
    // so every probe for the rest of the process's life paid the `EnumWindows`
    // sweep — the fallback, on the hot 5 s detector path, forever after any
    // recovery. Refill it here, at the one site that knows a fresh window
    // exists.
    //
    // Deliberately via `main_hwnd()`'s own sweep rather than `win.hwnd()`:
    // this is NOT the UI thread (we are back on an async task after
    // `spawn_blocking`), so `Window::hwnd()` here would be the unbounded
    // event-loop getter — `getter!` → `rx.recv()` with no timeout — which is
    // exactly what this whole module is not allowed to do off the main thread.
    // The sweep reads only the window table (no `SendMessage`), memoizes what
    // it finds, and simply reports `None` if the new window is not visible
    // yet, in which case the next detector tick re-resolves.
    match crate::ui_thread_probe::main_hwnd() {
        Some(hwnd) => info!("UI recovery: re-cached main-window HWND {hwnd:#x} after recreate"),
        None => warn!(
            "UI recovery: could not re-resolve a main-window HWND after recreate — the \
             native-liveness probe will retry on its next tick"
        ),
    }
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
    attach_process_failed(window, ProcessFailedRole::MainWindow);
}

/// [`attach_process_failed_handler`] for a **non-main** webview.
///
/// Wired at all three non-main builder sites:
/// `commands::terminal_windows::build_pop_out_webview` (`term-N`),
/// `click_overlay::initialize_overlay`, and
/// `commands::project_preview::open_project_preview`. Until plan
/// `2026-08-10-popout-webview2-creation-failure` Phase 3 the subscription had
/// exactly two call sites, both on a main window, so a pop-out whose webview
/// *died* was as silent as one that never got built. This closes that gap one
/// rung up from the build-time [`verify_window_has_a_webview`] probe those same
/// three sites now run.
#[cfg(windows)]
pub fn attach_non_main_process_failed_handler(window: &tauri::WebviewWindow) {
    attach_process_failed(window, ProcessFailedRole::NonMain);
}

/// Which window the subscription is on, and therefore what a `ProcessFailed`
/// event on it means.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessFailedRole {
    /// The runner's `"main"` window. Its death takes the whole UI with it, so
    /// the event drives [`trigger_ui_recovery`].
    MainWindow,
    /// Any other webview — a `term-N` pop-out, the click overlay, the project
    /// preview.
    ///
    /// **Never** drives [`trigger_ui_recovery`]: that ladder rebuilds the MAIN
    /// window ([`recreate_main_window`]), so routing a pop-out's crash into it
    /// would tear down and recreate a window that never failed.
    ///
    /// **And never writes `ui_error`** — see
    /// [`is_terminal_for_a_non_main_webview`] and the "no backend writer"
    /// section of [`crate::ui_error`]. The response is a log line whose LEVEL
    /// carries the classification: `error!` for a webview that is genuinely
    /// dead, the pre-existing `warn!` for the transient classes WebView2
    /// recovers from by itself.
    NonMain,
}

/// Is this failure kind the **death** of a non-main webview, or a transient
/// state it recovers from?
///
/// Deliberately *not* [`is_no_op_reason`]. That predicate answers a different
/// question — "does the MAIN window's recovery ladder need to act?" — and
/// derives from [`plan_action`], which maps only
/// `FrameRenderExited | Ancillary(_)` to [`RecoveryAction::None`] because
/// everything else is worth a *reload* on the main window. A non-main webview
/// has no reload driver, so borrowing that predicate silently promoted
/// [`ProcessFailureKind::RenderUnresponsive`] — WebView2's "the renderer has
/// not answered a ping", routinely followed by the renderer answering — into a
/// reported incident. That over-trigger is half of why the first cut of this
/// code latched an otherwise-healthy runner into `derived_status: "errored"`.
///
/// So: a webview whose **browser** or **renderer process exited** is dead and
/// stays dead until something rebuilds it — that is worth an `error!`. An
/// unresponsive renderer, an out-of-process iframe's renderer, and the
/// GPU/utility/sandbox helpers are all self-healing, and stay at `warn!`.
#[cfg(windows)]
fn is_terminal_for_a_non_main_webview(kind: ProcessFailureKind) -> bool {
    matches!(
        kind,
        ProcessFailureKind::BrowserExited | ProcessFailureKind::RenderExited
    )
}

#[cfg(windows)]
fn attach_process_failed(window: &tauri::WebviewWindow, role: ProcessFailedRole) {
    use tauri::Manager;

    // Never attach on a runner that was launched headless. `main.rs` does not
    // call this in server mode, but the guard is kept local so the invariant
    // survives a future caller.
    if is_server_mode() {
        return;
    }

    let app = window.app_handle().clone();
    let label = window.label().to_string();

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

        // The event handler below is `move` and takes `label`; keep a copy for
        // this closure's own attach/failure log line.
        let label_for_log = label.clone();

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
                    window = %label,
                    kind = kind.as_str(),
                    raw_kind = raw,
                    "WebView2 ProcessFailed"
                );

                match role {
                    ProcessFailedRole::MainWindow => {
                        // We are on the WebView2 UI thread — never block it.
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let outcome =
                                trigger_ui_recovery(&app, RecoveryReason::ProcessFailed(kind))
                                    .await;
                            info!(
                                kind = kind.as_str(),
                                outcome = outcome.as_str(),
                                "WebView2 ProcessFailed recovery finished"
                            );
                        });
                    }
                    ProcessFailedRole::NonMain => {
                        // No recovery ladder for a non-main webview, and no
                        // `ui_error` write — see `ProcessFailedRole::NonMain`.
                        // The response is the log level: the transient classes
                        // stay at the `warn!` above; a webview that is
                        // genuinely dead gets an `error!` naming the window,
                        // so it is greppable and unmistakable.
                        if is_terminal_for_a_non_main_webview(kind) {
                            error!(
                                window = %label,
                                kind = kind.as_str(),
                                raw_kind = raw,
                                "WebView2 process died on a non-main window — that window's UI \
                                 is dead until it is rebuilt. The runner itself is unaffected \
                                 and stays healthy."
                            );
                        }
                    }
                }
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
            Ok(()) => info!(
                window = %label_for_log,
                role = ?role,
                "WebView2 ProcessFailed handler attached"
            ),
            Err(e) => warn!(
                window = %label_for_log,
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

/// Non-Windows stub for the non-main subscription — same deliberate no-op as
/// [`attach_process_failed_handler`] above, for the same reason.
#[cfg(not(windows))]
pub fn attach_non_main_process_failed_handler(_window: &tauri::WebviewWindow) {
    debug!(
        "ProcessFailed subscription is Windows-only; a non-main webview's death is \
         invisible on this platform"
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
    /// The single-flight latch as of the reply. Redundant with a
    /// `"outcome": "recovery_wedged"` result and deliberately so: an operator
    /// reading this route wants the same `{inFlight, inFlightMs, wedged}` term
    /// `/health` publishes, on every outcome rather than only the bad one.
    pub ui_recovery: LatchReport,
    /// The window-swap latch, same term. A permanent `wedged: true` here is
    /// what a stuck `ExitVeto::VetoSwapInFlight` looks like from outside.
    pub window_swap: LatchReport,
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
///
/// # Reading the reply when recovery is wedged
///
/// `"outcome": "recovery_wedged"` with an `in_flight_ms` means a previous run
/// is still inside `build_main_window` and recovery is latched OFF. **This
/// route cannot break that open, and does not try**: there is no `force`
/// parameter, because a second `destroy()` + `build()` against the same label
/// while the first blocking thread is still in there is precisely the
/// concurrent-recreate race the single-flight exists to prevent. What it gives
/// you is the diagnosis — plus the same `ui_recovery` / `window_swap` terms
/// `/health` publishes and a matching `recovery_wedged` line in
/// `wedge-incidents.log`. The remedy for a wedged tao loop is the force-close
/// door, not another recreate.
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
        ui_recovery: recovery_latch_report(),
        window_swap: window_swap_report(),
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
            .chain([
                RecoveryReason::HeartbeatStale,
                RecoveryReason::Manual,
                RecoveryReason::NativeUiThreadHung,
            ])
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

    // ── the native message-loop rung (Phase 4) ─────────────────────────

    #[test]
    fn native_ui_thread_hang_never_attempts_a_recovery_action() {
        // The whole point of the rung: DETECT and SURFACE, never act. Both
        // ladder rungs dispatch through the loop that is wedged — `Reload` is
        // `window.eval`, `Recreate` is `destroy()` + a label-release poll that
        // would just burn `WINDOW_LABEL_RELEASE_TIMEOUT_MS` and fail. A future
        // edit that "helpfully" gives this reason an action would ship a
        // recovery that provably cannot run.
        let reason = RecoveryReason::NativeUiThreadHung;
        for attempt in 0..8 {
            assert_eq!(
                plan_action(reason, attempt),
                RecoveryAction::None,
                "attempt {attempt}"
            );
        }
        assert!(is_no_op_reason(reason));
    }

    #[test]
    fn native_ui_thread_hang_has_a_stable_distinct_reason_string() {
        // The string reaches logs and the `/ui/recover` JSON, and the
        // breadcrumb's `ui_thread_wedged` token is grepped alongside it.
        assert_eq!(
            RecoveryReason::NativeUiThreadHung.as_str(),
            "native_ui_thread_hung"
        );
        for other in [
            RecoveryReason::HeartbeatStale,
            RecoveryReason::Manual,
            RecoveryReason::ProcessFailed(ProcessFailureKind::BrowserExited),
        ] {
            assert_ne!(
                RecoveryReason::NativeUiThreadHung.as_str(),
                other.as_str(),
                "{other:?}"
            );
        }
    }

    #[test]
    fn native_hang_surfacing_latch_is_independent_of_the_exhaustion_latch() {
        // Two independent failures: a dead WebView2 host and a blocked host
        // thread. Neither may silence the other's one-shot user notice.
        EXHAUSTION_SURFACED.store(true, Ordering::SeqCst);
        NATIVE_HANG_SURFACED.store(false, Ordering::SeqCst);
        clear_native_ui_thread_hang();
        assert!(!NATIVE_HANG_SURFACED.load(Ordering::SeqCst));
        assert!(
            EXHAUSTION_SURFACED.load(Ordering::SeqCst),
            "clearing the native-hang latch must not clear the exhaustion latch"
        );
        // Leave the shared statics as we found them for the other tests.
        EXHAUSTION_SURFACED.store(false, Ordering::SeqCst);
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

    /// FINDING 2, stated as the arithmetic that caused it.
    ///
    /// `emergency_quit::request_force_close` neither closes nor hides the main
    /// window, so at `RunEvent::ExitRequested` the window is alive. Without
    /// quit-intent that is `VetoWindowAlive` and `api.prevent_exit()` — so
    /// `app_handle.exit(0)` was refused on EVERY force-close, healthy runners
    /// included, `embedded_pg::stop_on_exit()` never ran, and the hard exit one
    /// `FORCE_EXIT_MARGIN` later orphaned a `postgres` holding the data dir and
    /// port. The fix is upstream of this function — force-close now calls
    /// `mark_app_quitting()` — and this pins why that call is load-bearing
    /// rather than tidy.
    #[test]
    fn force_close_without_the_quitting_flag_would_be_vetoed() {
        // The shape of a force-close BEFORE the fix: no quit intent, no swap,
        // main window still on screen.
        assert_eq!(
            decide_exit_veto(false, false, true),
            ExitVeto::VetoWindowAlive,
            "this is the veto that orphaned PostgreSQL on every force-close"
        );
        // …and with the flag the force-close path now sets first.
        assert_eq!(
            decide_exit_veto(true, false, true),
            ExitVeto::AllowQuitRequested
        );
        assert!(!decide_exit_veto(true, false, true).is_veto());
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
        // The wedge threshold sits between the two: longer than any single
        // healthy run, shorter than the incident reset. Its full derivation is
        // pinned by `recovery_wedge_threshold_cannot_drift_from_the_ladder`.
        assert!(RECOVERY_WEDGE_AFTER_MS > worst_case);
        assert!(RECOVERY_WEDGE_AFTER_MS < RECOVERY_ATTEMPT_RESET_MS);
    }

    // ── the in-flight latch, with an injected clock ────────────────────

    #[test]
    fn latch_reports_the_holders_age_from_the_injected_clock() {
        // The whole Phase-1 point: the latch knows WHEN it was taken, so a
        // reader can age it. No wall clock is consulted anywhere below.
        let latch = InFlightLatch::new();
        assert_eq!(latch.in_flight_age_ms(T0), None, "a free latch has no age");
        assert!(!latch.is_held());

        assert_eq!(latch.try_take(T0), Ok(()));
        assert!(latch.is_held());
        assert_eq!(latch.in_flight_age_ms(T0), Some(0));
        assert_eq!(latch.in_flight_age_ms(T0 + 1), Some(1));
        assert_eq!(latch.in_flight_age_ms(T0 + 250_000), Some(250_000));

        latch.release();
        assert!(!latch.is_held());
        assert_eq!(latch.in_flight_age_ms(T0 + 250_000), None);
    }

    #[test]
    fn latch_is_single_flight_and_the_refusal_carries_the_age() {
        // The refusal is what `trigger_ui_recovery` turns into either
        // `already_in_progress` or `Wedged` — so it has to carry the number
        // that discriminates them.
        let latch = InFlightLatch::new();
        assert_eq!(latch.try_take(T0), Ok(()));
        assert_eq!(latch.try_take(T0 + 200), Err(200), "a healthy overlap");
        assert_eq!(
            latch.try_take(T0 + RECOVERY_WEDGE_AFTER_MS),
            Err(RECOVERY_WEDGE_AFTER_MS),
            "a latched-off run"
        );
        // A refused take must not disturb the holder's stamp — otherwise the
        // heartbeat backstop, which retries on every stale tick, would reset
        // the age forever and no wedge could ever be reported.
        assert_eq!(latch.in_flight_age_ms(T0 + 1_000), Some(1_000));
    }

    #[test]
    fn latch_taken_in_the_first_millisecond_still_reads_as_held() {
        // The `+1` bias exists for exactly this: `now_ms == 0` is a legitimate
        // monotonic reading (the epoch is process start), and an unbiased
        // store would leave the latch indistinguishable from free while a
        // recreate was actually running.
        let latch = InFlightLatch::new();
        assert_eq!(latch.try_take(0), Ok(()));
        assert!(latch.is_held(), "taken at t=0 must not read as free");
        assert_eq!(latch.in_flight_age_ms(0), Some(0));
        assert_eq!(latch.try_take(0), Err(0));
    }

    #[test]
    fn latch_release_is_idempotent() {
        // `InProgressGuard` and `SwapGuard` both release on drop, including on
        // a panic or a dropped future; a double release must not resurrect a
        // stamp or panic.
        let latch = InFlightLatch::new();
        latch.try_take(T0).expect("free");
        latch.release();
        latch.release();
        assert!(!latch.is_held());
        assert_eq!(latch.try_take(T0 + 5), Ok(()));
    }

    #[test]
    fn take_unconditional_replaces_the_stamp_without_refusing() {
        // The swap latch is not a mutual-exclusion device (see
        // `InFlightLatch::take_unconditional`); its behaviour must stay exactly
        // what `store(true)` did, plus the age.
        let latch = InFlightLatch::new();
        latch.take_unconditional(T0);
        assert_eq!(latch.in_flight_age_ms(T0 + 10), Some(10));
        latch.take_unconditional(T0 + 10);
        assert_eq!(latch.in_flight_age_ms(T0 + 10), Some(0));
    }

    // ── wedged vs. a healthy overlap ───────────────────────────────────

    #[test]
    fn a_brief_overlap_is_not_a_wedge_but_a_latched_run_is() {
        // THE discrimination this plan exists for. Below the threshold the
        // report is an ordinary in-flight run; at or above it, `wedged`.
        assert_eq!(
            classify_latch(None),
            LatchReport {
                in_flight: false,
                in_flight_ms: None,
                wedged: false
            },
            "a free latch is never wedged"
        );
        for age in [0, 1, 200, RECOVERY_WEDGE_AFTER_MS - 1] {
            let r = classify_latch(Some(age));
            assert!(r.in_flight, "age {age}");
            assert_eq!(r.in_flight_ms, Some(age));
            assert!(!r.wedged, "age {age} is a healthy overlap, not a wedge");
        }
        for age in [
            RECOVERY_WEDGE_AFTER_MS,
            RECOVERY_WEDGE_AFTER_MS + 1,
            RECOVERY_ATTEMPT_RESET_MS,
            u64::MAX,
        ] {
            let r = classify_latch(Some(age));
            assert!(r.in_flight && r.wedged, "age {age} must report wedged");
            assert_eq!(r.in_flight_ms, Some(age));
        }
    }

    #[test]
    fn wedged_is_a_distinct_outcome_from_the_already_in_progress_skip() {
        // The 2026-08-06 defect stated as an assertion: these two used to be
        // the same bytes on every surface. `in_flight_ms` is the field that
        // could not be carried by `Skipped { why: &'static str }` at all, which
        // is why this is a variant rather than another reason string.
        let overlap = RecoveryOutcome::Skipped {
            why: "already_in_progress",
        };
        let wedged = RecoveryOutcome::Wedged {
            in_flight_ms: RECOVERY_WEDGE_AFTER_MS + 7,
        };
        assert_ne!(overlap.as_str(), wedged.as_str());
        assert_eq!(wedged.as_str(), "recovery_wedged");
        assert_ne!(overlap, wedged);

        // …and on the wire, where the operator actually reads it.
        let json = serde_json::to_value(&wedged).expect("serialize");
        assert_eq!(json["outcome"], "recovery_wedged");
        assert_eq!(json["in_flight_ms"], RECOVERY_WEDGE_AFTER_MS + 7);
        let skipped_json = serde_json::to_value(&overlap).expect("serialize");
        assert_eq!(skipped_json["outcome"], "skipped");
        assert_eq!(skipped_json["why"], "already_in_progress");
        assert!(
            skipped_json.get("in_flight_ms").is_none(),
            "the overlap skip carries no age — that is the whole difference"
        );
    }

    #[test]
    fn every_recovery_outcome_has_a_distinct_stable_string() {
        // These strings reach logs, `/ui/recover` and the breadcrumb grep.
        let all = [
            RecoveryOutcome::Skipped { why: "server_mode" },
            RecoveryOutcome::Reloaded,
            RecoveryOutcome::Recreated,
            RecoveryOutcome::Exhausted { attempts: 3 },
            RecoveryOutcome::Wedged { in_flight_ms: 1 },
            RecoveryOutcome::Failed {
                detail: "x".to_string(),
            },
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a.as_str(), b.as_str(), "{a:?} vs {b:?}");
            }
        }
    }

    // ── the wedge threshold, derived not invented ──────────────────────

    #[test]
    fn recovery_wedge_threshold_cannot_drift_from_the_ladder() {
        // Both ends of `RECOVERY_WEDGE_AFTER_MS`, pinned against the constants
        // it is derived from — so nobody can retune the backoff, the label
        // timeout or the pong deadline and silently turn healthy runs into
        // reported wedges (or the reverse).

        // The bounded costs a single healthy run can pay, summed.
        let bounded = RECOVERY_BACKOFF_MAX_MS      // longest single-run backoff
            + WINDOW_LABEL_RELEASE_TIMEOUT_MS      // label-release poll
            + RECREATE_PONG_DEADLINE_MS; // post-recreate pong watch
        assert!(
            RECOVERY_WEDGE_AFTER_MS > bounded,
            "wedge threshold {RECOVERY_WEDGE_AFTER_MS}ms must exceed every bounded cost of a \
             healthy run ({bounded}ms), or a slow-but-healthy recreate reports as wedged"
        );
        // …and the allowance over that is the cold-profile build, which is
        // deliberately unbounded. It is one more `RECOVERY_BACKOFF_MAX_MS`.
        assert_eq!(
            RECOVERY_WEDGE_AFTER_MS - bounded,
            RECOVERY_BACKOFF_MAX_MS,
            "the build allowance must stay derived from RECOVERY_BACKOFF_MAX_MS"
        );

        // The upper end: the loop guard must not declare a FRESH incident
        // before the wedge inside it was ever reported.
        assert!(
            RECOVERY_WEDGE_AFTER_MS < RECOVERY_ATTEMPT_RESET_MS,
            "wedge threshold {RECOVERY_WEDGE_AFTER_MS}ms must stay under the incident reset \
             window {RECOVERY_ATTEMPT_RESET_MS}ms"
        );

        // The pong deadline is borrowed from the UI-liveness calibration, not
        // invented here; and it must stay the SLACKER-than rung's junior.
        assert_eq!(
            RECREATE_PONG_DEADLINE_MS,
            crate::ui_error::UI_STALE_AFTER_MS
        );
        assert!(RECREATE_PONG_DEADLINE_MS < crate::ui_error::UI_DEAD_AFTER_MS);
    }

    // ── Phase 2: did the recreate actually produce a live UI? ───────────

    /// A recreate that finished at `T0`.
    const RECREATE_DONE: u64 = T0;

    #[test]
    fn a_pong_after_the_recreate_proves_the_rebuilt_ui_is_live() {
        assert_eq!(
            classify_recreate_pong(
                RECREATE_DONE + 1,
                RECREATE_DONE,
                0,
                RECREATE_PONG_DEADLINE_MS
            ),
            RecreatePongVerdict::Live
        );
        assert_eq!(
            classify_recreate_pong(
                RECREATE_DONE + 4_000,
                RECREATE_DONE,
                4_100,
                RECREATE_PONG_DEADLINE_MS
            ),
            RecreatePongVerdict::Live
        );
    }

    #[test]
    fn a_pong_from_before_the_recreate_proves_nothing() {
        // The bug this rung closes: the window that ponged is the one that was
        // just destroyed. Strictly-after, so even a same-millisecond stamp is
        // not credited.
        for stale in [0, 1, RECREATE_DONE - 1, RECREATE_DONE] {
            assert_eq!(
                classify_recreate_pong(stale, RECREATE_DONE, 0, RECREATE_PONG_DEADLINE_MS),
                RecreatePongVerdict::Waiting,
                "last_pong {stale} must not count as proof of the rebuilt window"
            );
            assert_eq!(
                classify_recreate_pong(
                    stale,
                    RECREATE_DONE,
                    RECREATE_PONG_DEADLINE_MS,
                    RECREATE_PONG_DEADLINE_MS
                ),
                RecreatePongVerdict::NoPong,
                "last_pong {stale} at the deadline is a failed recreate"
            );
        }
    }

    #[test]
    fn the_recreate_watch_waits_out_its_deadline_before_failing() {
        for elapsed in [0, 1, RECREATE_PONG_DEADLINE_MS - 1] {
            assert_eq!(
                classify_recreate_pong(0, RECREATE_DONE, elapsed, RECREATE_PONG_DEADLINE_MS),
                RecreatePongVerdict::Waiting,
                "elapsed {elapsed}"
            );
        }
        for elapsed in [
            RECREATE_PONG_DEADLINE_MS,
            RECREATE_PONG_DEADLINE_MS + 1,
            u64::MAX,
        ] {
            assert_eq!(
                classify_recreate_pong(0, RECREATE_DONE, elapsed, RECREATE_PONG_DEADLINE_MS),
                RecreatePongVerdict::NoPong,
                "elapsed {elapsed}"
            );
        }
    }

    /// **The guard the Phase-2 check is not allowed to break.**
    ///
    /// `ui_error::ui_stale`'s `last_pong > 0` test is what keeps a headless
    /// server-mode runner — and every runner's boot window — from reading as
    /// dead; `ui_stale_never_seen_is_not_stale_headless_server_mode_guard`
    /// (`ui_error.rs`) pins it and must stay green. This asserts the same
    /// property from the other side: the recreate check is RECREATE-SCOPED, it
    /// compares against the recreate's own completion instant, and it neither
    /// reads nor relaxes that global guard.
    #[test]
    fn the_recreate_check_does_not_relax_the_never_ponged_guard() {
        // The global guard, unchanged, at both calibrations.
        for age in [0, 1, crate::ui_error::UI_DEAD_AFTER_MS + 1, u64::MAX] {
            assert!(!crate::ui_error::ui_stale(
                0,
                age,
                crate::ui_error::UI_STALE_AFTER_MS
            ));
            assert!(!crate::ui_error::ui_stale(
                0,
                age,
                crate::ui_error::UI_DEAD_AFTER_MS
            ));
        }
        // And the recreate-scoped check, which reaches `NoPong` only because a
        // window was demonstrably just rebuilt in a NON-server-mode process
        // (hard gates 1 and 2 of `trigger_ui_recovery`) — never from staleness
        // alone, and never before its own deadline.
        assert_eq!(
            classify_recreate_pong(0, RECREATE_DONE, 0, RECREATE_PONG_DEADLINE_MS),
            RecreatePongVerdict::Waiting
        );
        assert_eq!(
            classify_recreate_pong(
                0,
                RECREATE_DONE,
                RECREATE_PONG_DEADLINE_MS,
                RECREATE_PONG_DEADLINE_MS
            ),
            RecreatePongVerdict::NoPong
        );
        // A server-mode runner cannot reach this code at all: gate 1 returns
        // first, and under `cargo test` gate 2 does.
        assert!(!is_server_mode());
        assert!(main_window_spec().is_none());
    }

    #[test]
    fn a_failed_recreate_verification_escalates_rather_than_latching_success() {
        // `NoPong` becomes `RecoveryOutcome::Failed`, which the loop guard
        // treats like any other failed rung — the next trigger escalates and
        // the budget eventually exhausts. It must NOT be `Recreated`, which
        // would report success over a blank window forever.
        let failed = RecoveryOutcome::Failed {
            detail: "no pong".to_string(),
        };
        assert_ne!(failed.as_str(), RecoveryOutcome::Recreated.as_str());
        // Recreate is the rung a repeat trigger lands on, so the escalation is
        // real rather than nominal.
        assert_eq!(
            plan_action(RecoveryReason::HeartbeatStale, 1),
            RecoveryAction::Recreate
        );
        assert_eq!(
            plan_action(RecoveryReason::HeartbeatStale, 2),
            RecoveryAction::Recreate
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

    // ── the non-main `ProcessFailed` classification ───────────────────────

    /// The over-trigger the 2026-08-19 review caught. `is_no_op_reason` is
    /// derived from `plan_action`, which keeps `RenderUnresponsive` actionable
    /// because the MAIN window can usefully be reloaded — so reusing it on a
    /// non-main webview promoted "the renderer has not answered a ping yet"
    /// into a reported incident. The two predicates must disagree here, and
    /// that disagreement is the point.
    #[cfg(windows)]
    #[test]
    fn an_unresponsive_renderer_is_not_a_dead_non_main_webview() {
        let kind = ProcessFailureKind::RenderUnresponsive;
        assert!(
            !is_terminal_for_a_non_main_webview(kind),
            "WebView2 routinely recovers an unresponsive renderer"
        );
        assert!(
            !is_no_op_reason(RecoveryReason::ProcessFailed(kind)),
            "control: the MAIN window's ladder DOES act on this kind — which is \
             exactly why the non-main path must not borrow that predicate"
        );
    }

    /// Genuine death, on both classes WebView2 does not restart by itself.
    #[cfg(windows)]
    #[test]
    fn a_dead_browser_or_renderer_process_is_a_dead_non_main_webview() {
        assert!(is_terminal_for_a_non_main_webview(
            ProcessFailureKind::BrowserExited
        ));
        assert!(is_terminal_for_a_non_main_webview(
            ProcessFailureKind::RenderExited
        ));
    }

    /// The self-healing classes: an out-of-process iframe's renderer and the
    /// GPU/utility/sandbox helpers. WebView2 restarts these itself and the
    /// top-level document keeps running.
    #[cfg(windows)]
    #[test]
    fn self_healing_subprocess_exits_are_not_a_dead_non_main_webview() {
        assert!(!is_terminal_for_a_non_main_webview(
            ProcessFailureKind::FrameRenderExited
        ));
        for raw in 4..=9 {
            assert!(
                !is_terminal_for_a_non_main_webview(ProcessFailureKind::from_raw(raw)),
                "ancillary subprocess {raw} is noise, not a dead webview"
            );
        }
    }

    // ── the invariant this whole plan is about, as a source-level guard ────

    /// Every `.rs` file under `src-tauri/src`, found from `CARGO_MANIFEST_DIR`
    /// rather than the CWD — a test executable run from the wrong directory
    /// would otherwise find nothing and pass **vacuously**.
    fn rust_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let name = path.display().to_string();
                    out.push((name, std::fs::read_to_string(&path).expect("read source")));
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        walk(&src, &mut out);
        assert!(
            out.len() > 20,
            "source walk found only {} files — the walk is broken, not the crate",
            out.len()
        );
        out
    }

    /// **The invariant the whole plan is about.**
    ///
    /// Plan `2026-08-10-popout-webview2-creation-failure`: a fourth
    /// `WebviewWindowBuilder` site existed that nobody knew about, it applied
    /// none of the main window's WebView2 environment options, and on a
    /// secondary runner its window came up with no webview at all — silently,
    /// because `build()` still returned `Ok`.
    ///
    /// Asserting "the four known sites are correct" would not have caught that
    /// bug and will not catch the next one. So this asserts the **general**
    /// rule instead: every `WebviewWindowBuilder::new` in the crate is followed
    /// — **within the next `WINDOW` lines**, not merely somewhere in the same
    /// file — by both halves of the contract: the shared environment options
    /// going in, and the post-build webview probe coming out. A fifth site, or
    /// an edit that drops either call from one of the four, fails here. All
    /// four of those mutations were replayed against this scan to confirm it
    /// detects them; a file-wide count, which is what this test did first, did
    /// not.
    ///
    /// `build_main_window` spells the options call as the private
    /// `apply_env_options(builder, webview_env_options(…))` because it is the
    /// source of the values rather than a consumer of them; the three non-main
    /// sites go through `apply_main_window_env_options`. Both spellings count.
    #[test]
    fn webview_builders_all_apply_the_shared_env_options_and_probe_the_result() {
        // Spelled with `concat!` so this line is not itself a match when the
        // scan reaches this file.
        const BUILDER: &str = concat!("WebviewWindowBuilder", "::new");
        const PROBE: &str = "verify_window_has_a_webview(";
        // `apply_main_window_env_options(` for the three non-main sites,
        // `webview_env_options(` for `build_main_window` itself.
        const ENV_OPTS: [&str; 2] = ["apply_main_window_env_options(", "webview_env_options("];
        // How far after a `WebviewWindowBuilder::new` the two required calls
        // must appear. Generous — the longest real chain today spans ~30 lines
        // — but bounded on purpose: a whole-file count is what the FIRST cut of
        // this test did, and it was VACUOUS. `terminal_windows.rs` names
        // `webview_env_options` twice more in its own test module, so removing
        // the production call still left the file-wide count above the
        // threshold and the mutation went undetected (checked by replaying the
        // scan over a mutated copy of the tree, 2026-08-19). Locality is what
        // makes the assertion mean anything.
        const WINDOW: usize = 80;

        let mut sites = 0usize;
        for (name, body) in rust_sources() {
            let lines: Vec<&str> = body.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") || !line.contains(BUILDER) {
                    continue;
                }
                sites += 1;

                // The lines that could carry the calls: comments are prose, and
                // a `fn` signature is the DEFINITION of one of these helpers,
                // not a call to it — counting either is how a guard goes
                // vacuous.
                let end = (i + WINDOW).min(lines.len());
                let scope: String = lines[i..end]
                    .iter()
                    .filter(|l| {
                        let t = l.trim_start();
                        !t.starts_with("//") && !t.starts_with("fn ") && !l.contains(" fn ")
                    })
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");

                assert!(
                    ENV_OPTS.iter().any(|n| scope.contains(n)),
                    "{name}:{} builds a webview window without applying the shared WebView2 \
                     environment options within {WINDOW} lines. Every builder must call \
                     `webview_recovery::apply_main_window_env_options` — without it Tauri \
                     forces `%LOCALAPPDATA%\\<identifier>` (the PRIMARY runner's profile root) \
                     on the window, and on a secondary runner it comes up with no webview at \
                     all (`HRESULT(0x8007139F)`).",
                    i + 1
                );
                assert!(
                    scope.contains(PROBE),
                    "{name}:{} builds a webview window without probing it within {WINDOW} \
                     lines. Every builder must call \
                     `webview_recovery::verify_window_has_a_webview` after `build()` — \
                     `build()` returns `Ok` for a window that has no webview at all.",
                    i + 1
                );
            }
        }

        assert!(
            sites >= 4,
            "expected at least the four known WebviewWindowBuilder sites, found {sites} — \
             the scan is broken, not the crate"
        );
    }
}
