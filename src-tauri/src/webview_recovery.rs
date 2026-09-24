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
/// is enough to distinguish "transient renderer crash" from "this build cannot
/// host a webview at all".
///
/// It counts TRIGGERS, not rungs, and since the 2026-09-19 plan one trigger
/// can run both rungs: a render-process crash starts on the reload and
/// escalates to recreate inside that same call when the reload is not verified
/// ([`trigger_ui_recovery`]). So the budget a `ProcessFailed` incident actually
/// spends is "reload→recreate, then recreate, then recreate" — the later
/// attempts arriving from the heartbeat backstop or a manual poke, which
/// [`plan_action`] starts on recreate directly.
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
/// **Rung-scoped, and deliberately so.** The predicate is "a MAIN-window pong
/// stamped strictly AFTER the recreate finished" ([`classify_rung_pong`]) — never a
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

/// How long a reloaded main webview has to prove it is running a UI again, by
/// ponging (plan
/// `2026-09-19-runner-render-process-crash-recovery-is-a-no-op-and-popout-pongs-mask-it`
/// Phase 2).
///
/// `ICoreWebView2::Reload()` returning `S_OK` means only that WebView2
/// ACCEPTED the navigation — the same "posted, not done" contract the `eval`
/// it replaced had, which is how the 2026-09-18 crash was "recovered" by a
/// no-op for five hours. So a reload is credited only when a main-labeled pong
/// from a DIFFERENT document lands strictly after the reload was accepted
/// ([`classify_rung_pong`]); without one inside this deadline the same call
/// escalates to recreate.
///
/// # Why this is NOT [`RECREATE_PONG_DEADLINE_MS`]
///
/// Because the two rungs fail differently when the deadline is too tight. A
/// recreate that times out reports `Failed` and hands the next trigger to the
/// loop guard; a reload that times out **escalates in-call**, and the price of
/// that escalation is a full destroy/rebuild of a window whose UI may simply
/// have been slow to boot. The 30 s borrowed from
/// [`crate::ui_error::UI_STALE_AFTER_MS`] is the "a live UI has checked in
/// within this long" calibration for a RUNNING frontend; a reload is a COLD
/// bundle boot against a WebView2 profile that has just lost its render
/// process, on a box loaded enough to have lost it. This file already argues
/// exactly that allowance for the cold-profile build probe in
/// [`RECOVERY_WEDGE_AFTER_MS`], so the reload rung takes the same number —
/// [`COLD_BUNDLE_BOOT_ALLOWANCE_MS`] — rather than inventing a third.
///
/// The ceiling on it is [`RECOVERY_ATTEMPT_RESET_MS`], through
/// [`RECOVERY_WEDGE_AFTER_MS`], of which this is one term;
/// `recovery_wedge_threshold_cannot_drift_from_the_ladder` pins both.
pub const RELOAD_PONG_DEADLINE_MS: u64 = COLD_BUNDLE_BOOT_ALLOWANCE_MS;

/// How long a COLD WebView2 bundle boot may take on a loaded box before
/// something is genuinely wrong.
///
/// # Why this is its own constant
///
/// Two costs in this file are the same question — "the frontend bundle is
/// loading from scratch against a cold WebView2 profile; how long do we
/// allow?" — and both used to be spelled [`RECOVERY_BACKOFF_MAX_MS`], which
/// answers an unrelated one ("how long may the loop guard sleep between
/// attempts?"). They were numerically equal, so retuning the backoff silently
/// retuned the reload deadline and the wedge threshold's build allowance with
/// it. Naming the shared meaning is what stops that: the backoff is now free
/// to move without touching either.
///
/// The two users, both of them cold boots:
///
/// * [`RELOAD_PONG_DEADLINE_MS`] — a reload is a cold bundle boot against a
///   profile that has just lost its render process.
/// * the trailing term of [`RECOVERY_WEDGE_AFTER_MS`] — the allowance for
///   `build_main_window`'s deliberately unbounded post-build probe.
pub const COLD_BUNDLE_BOOT_ALLOWANCE_MS: u64 = 60_000;

/// Poll interval while watching for a post-rung pong (either rung).
const RUNG_PONG_POLL_MS: u64 = 250;

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
/// * [`RELOAD_PONG_DEADLINE_MS`] — the bounded post-reload pong watch. A run
///   that reloads, hears nothing and escalates pays it IN FULL before the
///   recreate's own costs above, all under the one latch.
/// * [`COLD_BUNDLE_BOOT_ALLOWANCE_MS`] as the allowance for the *unbounded*
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
    + RELOAD_PONG_DEADLINE_MS
    + COLD_BUNDLE_BOOT_ALLOWANCE_MS;

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
/// * [`RecoveryReason::RendererMemoryPressure`] — the renderer-memory
///   self-watchdog (`crate::renderer_watchdog`), plan
///   `2026-06-09-runner-renderer-memory-watchdog-and-twin-slo` Phase 1. The
///   one reason raised **before** anything has failed: the renderer is alive
///   and the browser process is healthy, so this starts on the cheap
///   [`RecoveryAction::Reload`] rung, which is exactly the tear-down-the-
///   document reclaim the watchdog wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryReason {
    ProcessFailed(ProcessFailureKind),
    HeartbeatStale,
    Manual,
    NativeUiThreadHung,
    RendererMemoryPressure,
}

impl RecoveryReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProcessFailed(_) => "process_failed",
            Self::HeartbeatStale => "heartbeat_stale",
            Self::Manual => "manual",
            Self::NativeUiThreadHung => "native_ui_thread_hung",
            Self::RendererMemoryPressure => "renderer_memory_pressure",
        }
    }
}

/// One rung of the escalation ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// A native `ICoreWebView2::Reload()` of the existing webview (`eval` of
    /// `location.reload()` off Windows) — the cheap rung, and the recovery the
    /// WebView2 docs prescribe for `RENDER_PROCESS_EXITED`. Verified by a
    /// main-window pong and escalated to [`RecoveryAction::Recreate`] inside
    /// the same call when none arrives. **Useless when the browser process is
    /// gone**, so it is never selected for [`ProcessFailureKind::BrowserExited`].
    Reload,
    /// Destroy and rebuild the main `WebviewWindow`.
    Recreate,
    /// Log only. WebView2 handles this class itself; acting would spin.
    None,
}

/// Choose the rung for `reason` on 0-indexed `attempt`.
///
/// This picks the rung a run STARTS on. Both rungs are verified by a
/// main-window pong, and a reload that is not verified escalates to recreate
/// inside the same call — so escalation happens both within a call (reload →
/// recreate) and across attempts (a repeat trigger starts on recreate):
///
/// * **Reload is verified**, since plan
///   `2026-09-19-runner-render-process-crash-recovery-is-a-no-op-and-popout-pongs-mask-it`
///   Phase 2. It is a native `ICoreWebView2::Reload()` (the WebView2-documented
///   recovery for `RENDER_PROCESS_EXITED`; an injected `location.reload()` is
///   inert on the error page a dead renderer leaves behind), and
///   [`trigger_ui_recovery`] then requires a main-labeled pong stamped strictly
///   after the dispatch, within [`RELOAD_PONG_DEADLINE_MS`]. A dispatch error,
///   a WebView2 refusal, or no pong all escalate to recreate IN THE SAME CALL.
///   The old "second trigger" escalation could not be relied on: a
///   render-process crash raises exactly one `ProcessFailed`.
/// * **Recreate IS verified**, since Phase 2 of plan
///   `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure`.
///   [`trigger_ui_recovery`] reads `ui_bridge_last_pong` after a successful
///   rebuild and requires a pong stamped **strictly after** it
///   ([`classify_rung_pong`]), so a window that rebuilds blank reports
///   `Failed` and lets this ladder escalate instead of claiming success
///   forever.
///
/// Both reads are **rung-scoped**: each compares against its own rung's
/// instant and never relaxes the global `last_pong > 0` guard in
/// [`crate::ui_error::ui_stale`]. And both read a MAIN-scoped stamp —
/// `ui_bridge_last_pong` advances only on pongs labeled with the main
/// window's label (`crate::ui_error::ingest_window_pong`), so a live pop-out
/// cannot verify a rung that left the main window dead.
///
/// `attempt >= 1` → `Recreate` stays: it is the path for the heartbeat
/// backstop and manual pokes re-entering an incident whose reload was spent.
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
        // * **Reload** is `with_webview` → `ICoreWebView2::Reload()`
        //   ([`reload_main_webview`]), and `with_webview` dispatches onto the
        //   very loop that is wedged.
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
        // Renderer death, an unresponsive renderer, a stale heartbeat, an
        // operator poke, or the memory watchdog acting BEFORE a death: try the
        // cheap rung first, escalate if asked again.
        //
        // `RendererMemoryPressure` belongs here rather than beside
        // `BrowserExited`: the browser process is alive, and the reload rung —
        // a native `ICoreWebView2::Reload()` that tears the document down — is
        // precisely the reclaim the watchdog is asking for. A recreate would
        // work too but costs a window rebuild, so it stays the escalation.
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

/// How many recovery attempts the loop guard has counted.
///
/// Read straight off the [`LoopGuard`] for the same reason
/// [`recovery_exhausted`] is: one source of truth. This accessor exists so the
/// shutdown path can name the webview's state without reaching into the mutex
/// itself — and so [`recover_ui_handler`] stops keeping a private second copy
/// of the same three lines.
pub fn recovery_attempts() -> u32 {
    LOOP_GUARD
        .lock()
        .map(|g| g.attempts())
        .unwrap_or_else(|p| p.into_inner().attempts())
}

/// Everything this module knows, rendered for ONE shutdown log line.
///
/// Plan `2026-08-19-session-info-dropdown-mount-gaps-remediation`, D3. Three
/// runner exits in ten minutes of UI-Bridge driving left nothing in the log to
/// tell them apart: no panic, no shutdown line, and — the part that cost the
/// day — no statement of whether the webview had reported a crash first. The
/// shutdown path asks this module directly rather than guessing from symptoms.
///
/// Deliberately a flat `key=value` string: it is written to
/// `runner-lifecycle.log`, which is grepped, not parsed.
pub fn shutdown_diagnostics() -> String {
    // `try_lock`, NOT `lock` — this is called from the window-close handler,
    // which runs on the tao/UI thread. Plan
    // `2026-08-19-runner-blocked-ui-thread-cannot-be-closed` exists because
    // blocking that thread is what made the X button do nothing, and a
    // diagnostic must never be able to cause the failure it is diagnosing.
    // Every holder of this mutex holds it for microseconds, so contention is
    // improbable — and if it happens, an honest `unknown` is the right answer
    // rather than a wait.
    let guard_state = match LOOP_GUARD.try_lock() {
        Ok(g) => format!(
            "recovery_attempts={} recovery_exhausted={}",
            g.attempts(),
            g.is_exhausted()
        ),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            let g = p.into_inner();
            format!(
                "recovery_attempts={} recovery_exhausted={} (loop guard poisoned)",
                g.attempts(),
                g.is_exhausted()
            )
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            "recovery_attempts=unknown recovery_exhausted=unknown \
             (loop guard held — a recovery is in flight right now)"
                .to_string()
        }
    };
    format!(
        "{guard_state} recovery_in_progress={} window_swap_in_progress={} server_mode={}",
        RECOVERY_LATCH.is_held(),
        WINDOW_SWAP_LATCH.is_held(),
        is_server_mode(),
    )
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
    /// The main webview was reloaded in place.
    ///
    /// `verified: true` — a main-labeled pong from a DIFFERENT document landed
    /// strictly after WebView2 ACCEPTED the reload (not merely after it was
    /// dispatched: `with_webview` only enqueues, so the watch re-baselines
    /// onto the acceptance instant — [`watch_for_main_pong`]). Both conjuncts
    /// matter: the pre-reload page keeps ponging on the same window label
    /// until the new document replaces it, so a timestamp alone would let the
    /// very page the reload was meant to replace credit it.
    /// `verified: false` — the pong stamp is not readable in this process (no
    /// managed `AppState`), so the reload is UNKNOWN, deliberately not a
    /// failure, the same stance recreate takes. A reload that was dispatched
    /// and heard NOTHING never reports this variant: it escalates to recreate,
    /// whose outcome then carries `escalated_from_reload`.
    Reloaded { verified: bool },
    /// The main window was destroyed and rebuilt, and (unless unverifiable)
    /// the rebuilt UI ponged. `escalated_from_reload` is `Some` when this run
    /// started on the reload rung and handed over because the reload did not
    /// take — the fact that tells "reload dispatched, no pong, escalated" apart
    /// from a run that planned recreate from the start.
    Recreated {
        escalated_from_reload: Option<ReloadEscalation>,
    },
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
    /// A rung was attempted and failed. `escalated_from_reload` as for
    /// [`RecoveryOutcome::Recreated`]: `Some` when a reload was tried first in
    /// this same run and did not take.
    Failed {
        detail: String,
        escalated_from_reload: Option<ReloadEscalation>,
    },
}

/// Why a run that started on the reload rung escalated to recreate inside the
/// same call. Carried on the recreate's outcome so every surface (`/ui/recover`,
/// the recovery log) says which rung actually did the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum ReloadEscalation {
    /// The reload could not even be dispatched (no main window, a
    /// `with_webview` error, or an `eval` error off Windows). Hard evidence.
    DispatchFailed { detail: String },
    /// WebView2 ran the dispatch and REFUSED it: `CoreWebView2()` or
    /// `Reload()` returned an error on the UI thread, or the dispatched
    /// closure was dropped without running.
    Refused { detail: String },
    /// No qualifying main-window pong landed within `deadline_ms` of the
    /// **watch starting** — the reloaded page is not running a UI.
    ///
    /// `accepted` is WebView2's own answer, latched by [`AcceptanceWatch`],
    /// and it is the difference between two genuinely different incidents:
    ///
    /// * `true` — WebView2 ran `Reload()` and accepted the navigation, and
    ///   still nothing ponged. The reload took and the page came up dead.
    /// * `false` — WebView2 never answered at all (the UI thread has not run
    ///   the dispatched closure, or this platform's `eval` fallback has no
    ///   answer to give), so whether the reload ever ran is UNKNOWN. Saying
    ///   "accepted" here would be a claim nothing in this process can make,
    ///   and this variant is rendered by [`Self::describe`] onto an
    ///   operator-visible incident line.
    ///
    /// `deadline_ms` is measured from the START of the pong watch, not from
    /// the dispatch and not from the acceptance: a `Rebaseline` shortens the
    /// remaining wait rather than extending the total, so the whole watch
    /// stays inside the one term [`RECOVERY_WEDGE_AFTER_MS`] budgets for it.
    NoPong { deadline_ms: u64, accepted: bool },
}

impl ReloadEscalation {
    /// One-line human rendering for logs.
    pub fn describe(&self) -> String {
        match self {
            Self::DispatchFailed { detail } => format!("reload could not be dispatched: {detail}"),
            Self::Refused { detail } => format!("WebView2 refused the reload: {detail}"),
            Self::NoPong {
                deadline_ms,
                accepted: true,
            } => format!(
                "reload accepted, but no main-window pong from a new document within \
                 {deadline_ms}ms of the watch"
            ),
            Self::NoPong {
                deadline_ms,
                accepted: false,
            } => format!(
                "reload dispatched but WebView2 never answered it, and no main-window pong \
                 from a new document within {deadline_ms}ms of the watch"
            ),
        }
    }
}

impl RecoveryOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Skipped { .. } => "skipped",
            Self::Reloaded { .. } => "reloaded",
            Self::Recreated { .. } => "recreated",
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
    //
    //    Verified, not assumed (plan 2026-09-19-runner-render-process-crash-
    //    recovery-is-a-no-op-and-popout-pongs-mask-it, Phase 2): a render-
    //    process crash raises ONE `ProcessFailed`, so a reload that silently
    //    does nothing used to end the ladder — 5 h of sad tab on 2026-09-18.
    //    Anything short of a main-window pong from a NEW document, landing
    //    after WebView2 accepted the call, escalates to recreate right here,
    //    in this call. `reload_dispatched_ms` is only the opening baseline:
    //    `with_webview` enqueues, so the watch re-baselines onto the
    //    acceptance instant the moment WebView2 answers.
    let mut escalated_from_reload: Option<ReloadEscalation> = None;
    if action == RecoveryAction::Reload {
        // Both baselines are read BEFORE the dispatch: the instant, and the
        // identity of the document that is live right now. Reading the
        // identity afterwards would let a page that came up in between become
        // its own baseline.
        let document_at_dispatch = crate::ui_error::main_document_nonce();
        let reload_dispatched_ms = now_ms();
        // The sequencing itself is `run_reload_rung`, which is unit-tested
        // over a canned watch; everything Tauri-shaped is inside the future it
        // is handed.
        let last_pong = ui_bridge_last_pong(app);
        let rung = run_reload_rung(reload_main_webview(app).map(|dispatch| {
            verify_reload_took(
                last_pong,
                reload_dispatched_ms,
                document_at_dispatch,
                crate::ui_error::main_document_nonce,
                dispatch,
            )
        }))
        .await;
        match rung {
            Ok(outcome) => return outcome,
            Err(escalation) => {
                warn!(
                    escalation = %escalation.describe(),
                    "UI recovery: the reload did not take — escalating to recreate in this same call"
                );
                escalated_from_reload = Some(escalation);
            }
        }
    }

    // ── Rung 2: recreate.
    //
    //    The document identity that is live BEFORE the window is destroyed,
    //    for the same reason the reload rung reads it before dispatching.
    let document_before_recreate = crate::ui_error::main_document_nonce();
    match recreate_main_window(app).await {
        Ok(()) => {
            // Phase 2 (plan
            // `2026-08-06-runner-webview-recovery-wedge-and-disk-pressure`).
            // `Ok` here means the window was rebuilt and HAS a webview — it
            // does NOT mean a UI is running inside it. Require a main-window
            // pong stamped strictly after this instant, so a rebuild that comes
            // up blank reports `Failed` and lets the loop guard escalate on the
            // next trigger, instead of claiming `Recreated` over a dead window.
            //
            // The document-identity conjunct rides along: a rebuilt window
            // runs a fresh bundle load, so its nonce differs from the one the
            // destroyed window was ponging. It cannot weaken this rung — a
            // main window that never ponged an identity has no recorded nonce,
            // and `document_identity_changed` reads that as "no predecessor to
            // be fooled by" rather than as a refusal.
            let recreate_done_ms = now_ms();
            match watch_for_main_pong(
                ui_bridge_last_pong(app),
                recreate_done_ms,
                document_before_recreate,
                RECREATE_PONG_DEADLINE_MS,
                crate::ui_error::main_document_nonce,
                || RungTick::Continue,
            )
            .await
            {
                RungWatch::Settled(RungPongVerdict::Live) => {
                    info!("UI recovery: main window recreated and the rebuilt UI has ponged");
                    RecoveryOutcome::Recreated {
                        escalated_from_reload,
                    }
                }
                RungWatch::Settled(RungPongVerdict::Unverifiable) => {
                    // UNKNOWN is not failure: with no managed `AppState` there
                    // is no pong stamp to read, and inventing a verdict from
                    // that absence would fail every healthy recreate.
                    warn!(
                        "UI recovery: main window recreated, but the pong stamp is not \
                         readable in this process — recreate reported as done (UNKNOWN, \
                         deliberately not a failure)"
                    );
                    RecoveryOutcome::Recreated {
                        escalated_from_reload,
                    }
                }
                // The watch loop resolves `Waiting` itself, and this watch has
                // no abort source; both matched rather than assumed.
                RungWatch::Settled(RungPongVerdict::NoPong | RungPongVerdict::Waiting)
                | RungWatch::Aborted(_) => {
                    let detail = format!(
                        "main window rebuilt, but no main-window UI-Bridge pong arrived within \
                         {RECREATE_PONG_DEADLINE_MS}ms of the recreate — the rebuilt window \
                         has no live UI"
                    );
                    error!(detail = %detail, "UI recovery: the recreate produced no live UI");
                    RecoveryOutcome::Failed {
                        detail,
                        escalated_from_reload,
                    }
                }
            }
        }
        Err(e) => {
            error!(error = %e, "UI recovery: main window recreate FAILED");
            RecoveryOutcome::Failed {
                detail: e,
                escalated_from_reload,
            }
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

/// Verdict of a post-rung pong watch (reload or recreate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RungPongVerdict {
    /// A main-window pong stamped strictly after the rung landed — the main
    /// webview is demonstrably running a UI again.
    Live,
    /// No qualifying pong yet, and the deadline has not passed: keep waiting.
    /// Only [`classify_rung_pong`] returns this; the watch loop resolves it.
    Waiting,
    /// The deadline passed with no pong after the rung.
    NoPong,
    /// The pong stamp could not be read in this process. UNKNOWN — never a
    /// failure. Only the watch loop returns this.
    Unverifiable,
}

/// Did the rung (a reload or a recreate) produce a live main-window UI? Pure,
/// so it is testable without an `AppHandle` (there is no way to build one in a
/// unit test — see `server_mode_makes_recovery_inert`).
///
/// `last_pong_ms` is `AppState::ui_bridge_last_pong`, which only a pong
/// labeled with the MAIN window's label advances
/// (`crate::ui_error::ingest_window_pong`). That scoping is load-bearing here:
/// before it, a live pop-out terminal window's pong verified a rung that had
/// left the main window dead.
///
/// # Rung-scoped, and why that matters
///
/// The predicate is `last_pong_ms > rung_done_ms` — **strictly** after. A pong
/// from before the rung proves nothing about the page (or window) that
/// replaced it, and a `>=` would let one land on the same millisecond boundary.
///
/// # Why a timestamp alone is not enough
///
/// `document_changed` is the second conjunct, and it is load-bearing for the
/// RELOAD rung: `Reload()` only accepts a navigation, so the pre-reload
/// document keeps ponging (the frontend's unconditional 3 s safety-net pong)
/// until the new one replaces it. A pong stamped after the reload instant can
/// therefore come from the very page the reload was meant to replace — which
/// is a "verified" reload that verified nothing, the defect class this whole
/// plan exists to close. The frontend mints one nonce per document
/// (`DOCUMENT_NONCE`), Rust records the main window's current one, and the
/// caller passes whether it CHANGED since the rung was dispatched
/// ([`crate::ui_error::document_identity_changed`], which refuses every
/// absence except "nothing was ponging before the rung").
///
/// This is deliberately NOT a relaxation of the global `last_pong > 0` guard in
/// [`crate::ui_error::ui_stale`]. That guard is what keeps a headless
/// server-mode runner — which never mounts a webview at all — and every
/// runner's boot window from reading as dead, and
/// `ui_stale_never_seen_is_not_stale_headless_server_mode_guard` pins it. Here,
/// `last_pong_ms == 0` simply fails the strict comparison like any other stamp
/// older than the rung: it waits, and then reports `NoPong`. That is the
/// honest answer in this scope only, because reaching it means a rung was just
/// run in a process that is not in server mode (hard gate 1 of
/// [`trigger_ui_recovery`]) and that had a recorded [`MainWindowSpec`]
/// (hard gate 2).
///
/// `elapsed_ms` is measured on the MONOTONIC clock ([`latch_now_ms`]), not from
/// two wall-clock reads: an NTP step backwards during the watch would otherwise
/// saturate the age to 0 and wait forever.
pub fn classify_rung_pong(
    last_pong_ms: u64,
    rung_done_ms: u64,
    document_changed: bool,
    elapsed_ms: u64,
    deadline_ms: u64,
) -> RungPongVerdict {
    if last_pong_ms > rung_done_ms && document_changed {
        return RungPongVerdict::Live;
    }
    if elapsed_ms >= deadline_ms {
        return RungPongVerdict::NoPong;
    }
    RungPongVerdict::Waiting
}

/// How a post-rung pong watch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RungWatch {
    /// [`classify_rung_pong`] settled (never `Waiting`), or the stamp was
    /// unreadable (`Unverifiable`).
    Settled(RungPongVerdict),
    /// The watch's abort source reported hard evidence the rung failed before
    /// the deadline — for the reload rung, WebView2 refusing the `Reload()`.
    Aborted(String),
}

/// What the rung's own signal source says on one tick of the pong watch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RungTick {
    /// Nothing new — keep waiting on the current baseline.
    Continue,
    /// The rung was ACCEPTED at this wall-clock instant. Move the pong
    /// baseline onto it: until acceptance, a dispatch has only been *enqueued*
    /// and the pre-rung document is still live and still ponging.
    Rebaseline { rung_done_ms: u64 },
    /// Hard evidence the rung failed — end the watch now rather than sitting
    /// out the whole deadline.
    Abort(String),
}

/// Watch `ui_bridge_last_pong` (MAIN-window pongs only) for `deadline_ms` and
/// settle [`classify_rung_pong`] against `rung_done_ms`.
///
/// # The seam, and why it is drawn here
///
/// `last_pong` is the stamp itself rather than an `AppHandle`, and
/// `document_now` is the identity reader rather than a direct call to
/// [`crate::ui_error::main_document_nonce`]. There is no way to build an
/// `AppHandle` in a unit test (see `server_mode_makes_recovery_inert`), so
/// while this function took one its whole body — the re-baseline, the
/// two-conjunct predicate, the post-settle read in [`verify_reload_took`] —
/// was unreachable from the suite: deleting any of them left it green. Both
/// parameters are what a caller already has (`ui_bridge_last_pong(app)`, and
/// the process-global nonce reader), so nothing about the production path
/// changes; the seam only moves the untestable Tauri lookup one layer out.
/// `None` means this process has no managed `AppState` — UNKNOWN, reported as
/// `Unverifiable`, never a failure.
///
/// `signal` is polled on every tick the verdict is still `Waiting`; it may end
/// the watch ([`RungTick::Abort`]) or move the baseline forward
/// ([`RungTick::Rebaseline`]).
///
/// `document_at_rung` is the nonce the main window was ponging BEFORE the rung
/// ran — read by the caller ahead of the dispatch, not here, because a page
/// that came up between the rung and this call would otherwise become its own
/// baseline and could never prove it is new. A rung is credited only by a pong
/// from a different document, and that baseline is deliberately NOT moved by a
/// `Rebaseline` either: acceptance says the navigation was accepted, not that a
/// new document exists, so re-reading the nonce there could only discard the
/// evidence.
///
/// `deadline_ms` is measured from the START of the watch, not from the
/// baseline, so a `Rebaseline` shortens the remaining wait rather than
/// extending the total: the whole watch stays inside the one term
/// [`RECOVERY_WEDGE_AFTER_MS`] budgets for it.
///
/// Runs inside the recovery latch, which is accounted for: each rung's
/// deadline is its own term in [`RECOVERY_WEDGE_AFTER_MS`].
async fn watch_for_main_pong(
    last_pong: Option<std::sync::Arc<AtomicU64>>,
    rung_done_ms: u64,
    document_at_rung: Option<String>,
    deadline_ms: u64,
    mut document_now: impl FnMut() -> Option<String>,
    mut signal: impl FnMut() -> RungTick,
) -> RungWatch {
    let Some(last_pong) = last_pong else {
        return RungWatch::Settled(RungPongVerdict::Unverifiable);
    };
    let started_ms = latch_now_ms();
    let mut baseline_ms = rung_done_ms;
    loop {
        // Stamp first, identity second: the ingest publishes them the other
        // way round, so this order can never pair a fresh pong with the
        // identity of the document that preceded it.
        let pong_ms = last_pong.load(Ordering::Relaxed);
        let document_changed = crate::ui_error::document_identity_changed(
            document_at_rung.as_deref(),
            document_now().as_deref(),
        );
        match classify_rung_pong(
            pong_ms,
            baseline_ms,
            document_changed,
            latch_now_ms().saturating_sub(started_ms),
            deadline_ms,
        ) {
            RungPongVerdict::Waiting => {
                match signal() {
                    RungTick::Abort(detail) => return RungWatch::Aborted(detail),
                    RungTick::Rebaseline {
                        rung_done_ms: accepted_ms,
                    } => {
                        debug!(
                            baseline_ms,
                            accepted_ms, "UI recovery: re-baselining the rung pong watch"
                        );
                        baseline_ms = accepted_ms;
                    }
                    RungTick::Continue => {}
                }
                tokio::time::sleep(std::time::Duration::from_millis(RUNG_PONG_POLL_MS)).await;
            }
            settled => return RungWatch::Settled(settled),
        }
    }
}

/// WebView2's answer to a dispatched `Reload()`.
///
/// Kept as a VALUE rather than consumed as a side effect, because the answer
/// can arrive after the pong watch has already settled — and a refusal that
/// lands one tick after a `Live` verdict is still evidence that the rung never
/// ran. Before this was a value it was thrown away twice over: the dispatch's
/// `let _ = tx.send(result)` on the UI thread, and the watch dropping its
/// receiver the moment it settled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReloadAcceptance {
    /// No answer yet (the UI thread has not run the closure), or this platform
    /// has none to give (the non-Windows `eval` path). UNKNOWN, never a
    /// refusal.
    Unanswered,
    /// WebView2 ran the call and accepted it, at this wall-clock instant.
    Accepted { at_ms: u64 },
    /// WebView2 refused it, or the dispatched closure was dropped unrun.
    Refused { detail: String },
}

/// What one `try_recv` of the acceptance channel means. Pure, so every arm is
/// testable without a webview. `None` = nothing to record yet (keep polling);
/// `Some` = the answer, after which the receiver must not be polled again — a
/// oneshot reads `Closed` once its value is taken, which would otherwise be
/// misread as "dropped unrun".
fn reload_acceptance_signal(
    polled: Result<Result<(), String>, tokio::sync::oneshot::error::TryRecvError>,
    now_ms: u64,
) -> Option<ReloadAcceptance> {
    use tokio::sync::oneshot::error::TryRecvError;
    match polled {
        // Not run yet — keep watching.
        Err(TryRecvError::Empty) => None,
        Ok(Ok(())) => Some(ReloadAcceptance::Accepted { at_ms: now_ms }),
        Ok(Err(detail)) => Some(ReloadAcceptance::Refused { detail }),
        Err(TryRecvError::Closed) => Some(ReloadAcceptance::Refused {
            detail: RELOAD_DROPPED_UNRUN.to_string(),
        }),
    }
}

/// The detail recorded when the dispatched reload closure was dropped without
/// ever running (its sender went away unsent).
const RELOAD_DROPPED_UNRUN: &str =
    "the reload closure was dropped without running — the webview never executed it";

/// Polls a [`ReloadDispatch`]'s acceptance channel and LATCHES the answer, so
/// it outlives the watch that was driving the polling.
struct AcceptanceWatch {
    rx: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    answer: ReloadAcceptance,
}

impl AcceptanceWatch {
    fn new(dispatch: ReloadDispatch) -> Self {
        Self {
            rx: dispatch.accepted,
            answer: ReloadAcceptance::Unanswered,
        }
    }

    /// Poll once and translate the answer into a watch tick. Idempotent after
    /// an answer has landed: the receiver is dropped and every later call is
    /// `Continue`.
    fn poll(&mut self, now_ms: u64) -> RungTick {
        let Some(rx) = self.rx.as_mut() else {
            return RungTick::Continue;
        };
        let Some(answer) = reload_acceptance_signal(rx.try_recv(), now_ms) else {
            return RungTick::Continue;
        };
        self.rx = None;
        let tick = match &answer {
            // Accepted is NOT success — it is the instant from which a pong
            // means anything at all, so the watch re-baselines onto it.
            ReloadAcceptance::Accepted { at_ms } => RungTick::Rebaseline {
                rung_done_ms: *at_ms,
            },
            ReloadAcceptance::Refused { detail } => RungTick::Abort(detail.clone()),
            ReloadAcceptance::Unanswered => RungTick::Continue,
        };
        self.answer = answer;
        tick
    }
}

/// A finished reload watch: the pong verdict, and WebView2's own answer to the
/// `Reload()` — including one that arrived after the verdict settled.
struct ReloadWatchResult {
    watch: RungWatch,
    acceptance: ReloadAcceptance,
}

/// [`watch_for_main_pong`] for the reload rung, with WebView2's own answer to
/// the `Reload()` call as both the re-baseline and the abort source.
async fn verify_reload_took(
    last_pong: Option<std::sync::Arc<AtomicU64>>,
    reload_dispatched_ms: u64,
    document_at_dispatch: Option<String>,
    document_now: impl FnMut() -> Option<String>,
    dispatch: ReloadDispatch,
) -> ReloadWatchResult {
    let mut acceptance = AcceptanceWatch::new(dispatch);
    let watch = watch_for_main_pong(
        last_pong,
        reload_dispatched_ms,
        document_at_dispatch,
        RELOAD_PONG_DEADLINE_MS,
        document_now,
        || acceptance.poll(now_ms()),
    )
    .await;
    // One last read AFTER the verdict. A refusal that raced the settling tick
    // used to die with the receiver; it is hard evidence the rung never ran,
    // and [`run_reload_rung`] escalates on it whatever the pong said.
    let _ = acceptance.poll(now_ms());
    ReloadWatchResult {
        watch,
        acceptance: acceptance.answer,
    }
}

/// The reload rung's sequencing, as one injectable seam.
///
/// `dispatch` is `Ok(<the pong watch>)` when the reload was dispatched, or
/// `Err(detail)` when it could not be — and the watch is a FUTURE, so the
/// error arm never starts one. Everything Tauri-shaped lives on the far side
/// of that future, which is what lets all four arms be tested without an
/// `AppHandle`:
///
/// * a verified pong ⇒ `Reloaded { verified: true }`
/// * an unreadable stamp ⇒ `Reloaded { verified: false }` (UNKNOWN, NOT a
///   failure — the same stance the recreate rung takes)
/// * no pong by the deadline ⇒ escalate ([`ReloadEscalation::NoPong`]),
///   carrying whether WebView2 ever ANSWERED the call — both `Accepted` and
///   `Unanswered` route here, and they are different incidents
/// * a refusal, from the watch's abort OR latched after it settled ⇒ escalate
/// * a dispatch error ⇒ escalate, without waiting on anything
///
/// A latched refusal outranks a `Live` pong on purpose. `Live` now requires a
/// pong from a DIFFERENT document, so the two answers should not co-occur; when
/// they do, WebView2's own "I did not run your call" is the answer about the
/// rung, and the ladder's failure mode to avoid is the one this plan opened
/// with — claiming a rung worked when it did nothing.
async fn run_reload_rung<F>(
    dispatch: Result<F, String>,
) -> Result<RecoveryOutcome, ReloadEscalation>
where
    F: std::future::Future<Output = ReloadWatchResult>,
{
    let watch = match dispatch {
        Ok(watch) => watch,
        // A dispatch error is hard evidence the webview is beyond a reload.
        Err(detail) => return Err(ReloadEscalation::DispatchFailed { detail }),
    };
    info!(
        deadline_ms = RELOAD_PONG_DEADLINE_MS,
        "UI recovery: reload dispatched into the existing webview — waiting for the reloaded \
         UI to pong"
    );
    let ReloadWatchResult { watch, acceptance } = watch.await;
    // Read before the move below: `NoPong` reports whether WebView2 ever
    // answered, because "accepted, then nothing ponged" and "never answered
    // at all" are different incidents and `describe()` puts this on an
    // operator-visible line.
    let accepted = matches!(acceptance, ReloadAcceptance::Accepted { .. });
    if let ReloadAcceptance::Refused { detail } = acceptance {
        return Err(ReloadEscalation::Refused { detail });
    }
    match watch {
        RungWatch::Settled(RungPongVerdict::Live) => {
            info!("UI recovery: main webview reloaded and the reloaded UI has ponged");
            Ok(RecoveryOutcome::Reloaded { verified: true })
        }
        RungWatch::Settled(RungPongVerdict::Unverifiable) => {
            warn!(
                "UI recovery: reload dispatched, but the pong stamp is not readable in this \
                 process — reload reported as done (UNKNOWN, deliberately not a failure)"
            );
            Ok(RecoveryOutcome::Reloaded { verified: false })
        }
        // The watch resolves `Waiting` itself; matched, not assumed.
        RungWatch::Settled(RungPongVerdict::NoPong | RungPongVerdict::Waiting) => {
            Err(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted,
            })
        }
        RungWatch::Aborted(detail) => Err(ReloadEscalation::Refused { detail }),
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

/// A reload that has been dispatched to the main webview.
///
/// `accepted` resolves once WebView2 has run the `Reload()` call on its UI
/// thread: `Ok` = accepted (NOT "the page is back" — only a pong says that),
/// `Err` = refused. `None` off Windows, where the `eval` fallback has no such
/// answer.
pub struct ReloadDispatch {
    accepted: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
}

impl ReloadDispatch {
    /// Wait up to `timeout` for WebView2's answer to the `Reload()` call.
    ///
    /// `Ok(Some(()))` = accepted; `Ok(None)` = no answer inside `timeout` (the
    /// UI thread has not run it yet — UNKNOWN, not a refusal) or no answer
    /// exists on this platform; `Err` = refused, or the dispatch was dropped
    /// unrun.
    pub async fn accepted_within(self, timeout: std::time::Duration) -> Result<Option<()>, String> {
        let Some(rx) = self.accepted else {
            return Ok(None);
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(()))) => Ok(Some(())),
            Ok(Ok(Err(detail))) => Err(detail),
            Ok(Err(_)) => Err(RELOAD_DROPPED_UNRUN.to_string()),
            Err(_) => Ok(None),
        }
    }
}

/// Reload the main webview in place — the recovery ladder's rung 1, and the
/// body of the `ui_bridge_reload_webview` command.
///
/// On Windows this is the native `ICoreWebView2::Reload()`, dispatched onto the
/// WebView2 UI thread through `with_webview` — the recovery the WebView2 docs
/// prescribe for `RENDER_PROCESS_EXITED`, and what the sad-tab page's own
/// Refresh button does. The `window.eval("location.reload()")` it replaces is
/// `ExecuteScript` into the main frame, which after a render-process crash
/// holds Chromium's error page: the injected script does not reload the app
/// (measured 2026-09-18 — `POST /ui/recover` answered `reloaded` and the UI
/// Bridge still timed out). Off Windows `eval` remains the only lever.
///
/// `Ok` means DISPATCHED. `with_webview` is asynchronous, so WebView2's answer
/// arrives on [`ReloadDispatch`]; and even an accepted reload proves nothing
/// until the reloaded page pongs — which is why [`trigger_ui_recovery`]
/// verifies it. `Err` (no main window, a dispatch error) is hard evidence.
pub fn reload_main_webview(app: &tauri::AppHandle) -> Result<ReloadDispatch, String> {
    use tauri::Manager;
    let label = qontinui_runner_lib::get_main_window_label();
    let window = app
        .get_webview_window(label)
        .ok_or_else(|| format!("main window '{label}' not found"))?;
    dispatch_reload(&window)
}

#[cfg(windows)]
fn dispatch_reload(window: &tauri::WebviewWindow) -> Result<ReloadDispatch, String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    window
        .with_webview(move |wv| {
            // SAFETY: two COM calls into WebView2 — `CoreWebView2()` (an
            // out-param getter on the controller) and `Reload()` (a no-argument
            // method). `with_webview` runs this closure on the WebView2 UI
            // thread, which is the thread these objects belong to, and the
            // controller is kept alive by the `PlatformWebview` for the whole
            // closure. Same plumbing and same `windows 0.61` type family as
            // `attach_process_failed` (the webview2-com re-exports), obtained
            // the same way: `wv.controller().CoreWebView2()`.
            let result: Result<(), String> = (|| unsafe {
                let core = wv
                    .controller()
                    .CoreWebView2()
                    .map_err(|e| format!("CoreWebView2(): {e}"))?;
                core.Reload()
                    .map_err(|e| format!("ICoreWebView2::Reload(): {e}"))
            })();
            if let Err(e) = &result {
                warn!(error = %e, "UI reload: WebView2 refused the native reload");
            }
            // The receiver may already be gone (the caller stopped watching);
            // the log line above is the record in that case.
            let _ = tx.send(result);
        })
        .map_err(|e| format!("with_webview (native reload dispatch): {e}"))?;
    Ok(ReloadDispatch { accepted: Some(rx) })
}

#[cfg(not(windows))]
fn dispatch_reload(window: &tauri::WebviewWindow) -> Result<ReloadDispatch, String> {
    window
        .eval("location.reload()")
        .map_err(|e| format!("eval(location.reload()): {e}"))?;
    Ok(ReloadDispatch { accepted: None })
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
    axum::Json(RecoverUiResponse {
        reason: RecoveryReason::Manual.as_str(),
        result,
        attempts: recovery_attempts(),
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
        // The rung a run STARTS on. Attempt 0 is the (now native, pong-verified)
        // reload; an unverified reload escalates to recreate inside that same
        // call (`trigger_ui_recovery`), so escalation no longer waits on a
        // second `ProcessFailed` a render-process crash never raises. The
        // attempt >= 1 → Recreate branch stays for the heartbeat backstop and
        // manual pokes re-entering the same incident.
        for kind in [
            ProcessFailureKind::RenderExited,
            ProcessFailureKind::RenderUnresponsive,
        ] {
            let reason = RecoveryReason::ProcessFailed(kind);
            assert_eq!(plan_action(reason, 0), RecoveryAction::Reload, "{kind:?}");
            for attempt in 1..MAX_RECOVERY_ATTEMPTS + 2 {
                assert_eq!(
                    plan_action(reason, attempt),
                    RecoveryAction::Recreate,
                    "{kind:?} attempt {attempt}"
                );
            }
        }
    }

    // ── Phase 2 (2026-09-19 plan): the reload rung is verified ──────────

    /// A reload that finished dispatching at `T0`.
    const RELOAD_DISPATCHED: u64 = T0;

    #[test]
    fn the_reload_rung_is_verified_by_the_same_rung_scoped_classifier() {
        // Generalised, not copied: the reload watch uses `classify_rung_pong`
        // with its own deadline.
        assert_eq!(
            classify_rung_pong(
                RELOAD_DISPATCHED + 1,
                RELOAD_DISPATCHED,
                true,
                0,
                RELOAD_PONG_DEADLINE_MS
            ),
            RungPongVerdict::Live
        );
        // A pong from the page the reload replaced (or the crashed renderer's
        // last one) proves nothing — strictly after, as for recreate.
        for stale in [0, RELOAD_DISPATCHED - 1, RELOAD_DISPATCHED] {
            assert_eq!(
                classify_rung_pong(stale, RELOAD_DISPATCHED, true, 0, RELOAD_PONG_DEADLINE_MS),
                RungPongVerdict::Waiting
            );
            assert_eq!(
                classify_rung_pong(
                    stale,
                    RELOAD_DISPATCHED,
                    true,
                    RELOAD_PONG_DEADLINE_MS,
                    RELOAD_PONG_DEADLINE_MS
                ),
                RungPongVerdict::NoPong,
                "no pong by the reload deadline must escalate, never read as reloaded"
            );
        }
    }

    /// The timestamp is only HALF the predicate. A pong stamped after the rung
    /// but carrying the SAME document identity is the pre-reload page still
    /// answering pings — the 2026-09-18 shape, where a reload that did nothing
    /// reported success. It must wait, and then escalate.
    #[test]
    fn a_pong_from_the_same_document_never_verifies_a_reload() {
        for deadline in [RECREATE_PONG_DEADLINE_MS, RELOAD_PONG_DEADLINE_MS] {
            assert_eq!(
                classify_rung_pong(RELOAD_DISPATCHED + 1, RELOAD_DISPATCHED, false, 0, deadline),
                RungPongVerdict::Waiting,
                "the page the rung was meant to replace must not credit it"
            );
            assert_eq!(
                classify_rung_pong(
                    RELOAD_DISPATCHED + 1,
                    RELOAD_DISPATCHED,
                    false,
                    deadline,
                    deadline
                ),
                RungPongVerdict::NoPong,
                "…and by the deadline that is an escalation, not a success"
            );
            // Only both halves together are evidence.
            assert_eq!(
                classify_rung_pong(RELOAD_DISPATCHED + 1, RELOAD_DISPATCHED, true, 0, deadline),
                RungPongVerdict::Live
            );
        }
    }

    /// Plan step 3: a NON-main pong landing after the rung instant must not
    /// verify it. `ui_bridge_last_pong` is main-scoped at its only writer, so
    /// a pop-out's pong never reaches the stamp the classifier reads.
    #[test]
    fn a_non_main_pong_after_the_rung_yields_no_pong() {
        // Wall-clock based rather than a fabricated constant: these ingests
        // stamp the PROCESS-GLOBAL event-loop clock, and a stamp from the past
        // would break `ui_error`'s `last_event_pong() >= now` assertion in a
        // sibling test. Every writer in this crate moves that clock forward.
        let rung_done = now_ms();
        let last_pong = AtomicU64::new(rung_done - 500);
        for deadline in [RECREATE_PONG_DEADLINE_MS, RELOAD_PONG_DEADLINE_MS] {
            // A pop-out (and an unlabeled caller) ponging after the rung.
            crate::ui_error::ingest_window_pong_at(
                &last_pong,
                Some("terminal-1"),
                None,
                true,
                "main",
                rung_done + 10,
            );
            crate::ui_error::ingest_window_pong_at(
                &last_pong,
                None,
                None,
                false,
                "main",
                rung_done + 20,
            );
            assert_eq!(
                classify_rung_pong(
                    last_pong.load(Ordering::Relaxed),
                    rung_done,
                    true,
                    deadline,
                    deadline
                ),
                RungPongVerdict::NoPong,
                "a pop-out pong must not verify a rung that left the main window dead"
            );
        }
        // …and the main window's own pong does.
        crate::ui_error::ingest_window_pong_at(
            &last_pong,
            Some("main"),
            None,
            false,
            "main",
            rung_done + 30,
        );
        assert_eq!(
            classify_rung_pong(
                last_pong.load(Ordering::Relaxed),
                rung_done,
                true,
                0,
                RELOAD_PONG_DEADLINE_MS
            ),
            RungPongVerdict::Live
        );
    }

    #[test]
    fn reload_acceptance_signal_reads_every_webview2_answer() {
        use tokio::sync::oneshot::error::TryRecvError;
        const AT: u64 = 4_242;
        // Not run yet: nothing to record, keep polling.
        assert_eq!(reload_acceptance_signal(Err(TryRecvError::Empty), AT), None);
        // Accepted: the ACCEPTANCE INSTANT is the answer — the dispatch before
        // it only enqueued the call, so it is what the pong watch re-baselines
        // onto.
        assert_eq!(
            reload_acceptance_signal(Ok(Ok(())), AT),
            Some(ReloadAcceptance::Accepted { at_ms: AT })
        );
        // Refused on the UI thread: WebView2's own words.
        assert_eq!(
            reload_acceptance_signal(Ok(Err("Reload(): E_FAIL".to_string())), AT),
            Some(ReloadAcceptance::Refused {
                detail: "Reload(): E_FAIL".to_string()
            })
        );
        // Dropped unrun: hard evidence too.
        assert_eq!(
            reload_acceptance_signal(Err(TryRecvError::Closed), AT),
            Some(ReloadAcceptance::Refused {
                detail: RELOAD_DROPPED_UNRUN.to_string()
            })
        );
    }

    /// The acceptance answer must LATCH, and a consumed receiver must never be
    /// re-read — a oneshot reads `Closed` once its value is taken, which would
    /// turn every accepted reload into a "dropped unrun" refusal one tick
    /// later.
    #[tokio::test]
    async fn the_acceptance_answer_latches_and_re_baselines_the_watch() {
        let dispatch = |r: Option<Result<(), String>>| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            match r {
                Some(r) => tx.send(r).unwrap(),
                None => drop(tx),
            }
            ReloadDispatch { accepted: Some(rx) }
        };

        // Accepted ⇒ re-baseline onto the acceptance instant, then silence.
        let mut w = AcceptanceWatch::new(dispatch(Some(Ok(()))));
        assert_eq!(w.poll(777), RungTick::Rebaseline { rung_done_ms: 777 });
        assert_eq!(w.answer, ReloadAcceptance::Accepted { at_ms: 777 });
        assert_eq!(w.poll(999), RungTick::Continue, "the answer is latched");
        assert_eq!(
            w.answer,
            ReloadAcceptance::Accepted { at_ms: 777 },
            "a consumed receiver must not degrade an acceptance into a refusal"
        );

        // Refused ⇒ abort, latched with the detail.
        let mut w = AcceptanceWatch::new(dispatch(Some(Err("E_FAIL".to_string()))));
        assert_eq!(w.poll(1), RungTick::Abort("E_FAIL".to_string()));
        assert_eq!(
            w.answer,
            ReloadAcceptance::Refused {
                detail: "E_FAIL".to_string()
            }
        );

        // Nothing sent yet ⇒ UNKNOWN, and still pollable.
        let (_tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let mut w = AcceptanceWatch::new(ReloadDispatch { accepted: Some(rx) });
        assert_eq!(w.poll(1), RungTick::Continue);
        assert_eq!(w.answer, ReloadAcceptance::Unanswered);

        // The `eval` fallback has no channel at all.
        let mut w = AcceptanceWatch::new(ReloadDispatch { accepted: None });
        assert_eq!(w.poll(1), RungTick::Continue);
        assert_eq!(w.answer, ReloadAcceptance::Unanswered);
    }

    /// Phase 2's central wiring, over a canned watch — the arm that decides
    /// whether an unverified reload actually escalates. Without this, deleting
    /// the verification and always answering `Reloaded { verified: true }`
    /// passed every other test in this file.
    #[tokio::test]
    async fn the_reload_rung_escalates_on_everything_but_a_pong() {
        let settled = |verdict| ReloadWatchResult {
            watch: RungWatch::Settled(verdict),
            acceptance: ReloadAcceptance::Accepted { at_ms: 1 },
        };

        // A verified pong is the ONE success.
        assert_eq!(
            run_reload_rung(Ok(async { settled(RungPongVerdict::Live) })).await,
            Ok(RecoveryOutcome::Reloaded { verified: true })
        );

        // No pong by the deadline ⇒ escalate, naming the deadline it waited.
        assert_eq!(
            run_reload_rung(Ok(async { settled(RungPongVerdict::NoPong) })).await,
            Err(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted: true
            })
        );
        // `Waiting` cannot escape the watch, but if it ever did it is NOT a
        // success either.
        assert_eq!(
            run_reload_rung(Ok(async { settled(RungPongVerdict::Waiting) })).await,
            Err(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted: true
            })
        );

        // A refusal the watch aborted on ⇒ escalate with WebView2's words.
        assert_eq!(
            run_reload_rung(Ok(async {
                ReloadWatchResult {
                    watch: RungWatch::Aborted("Reload(): E_FAIL".to_string()),
                    acceptance: ReloadAcceptance::Refused {
                        detail: "Reload(): E_FAIL".to_string(),
                    },
                }
            }))
            .await,
            Err(ReloadEscalation::Refused {
                detail: "Reload(): E_FAIL".to_string()
            })
        );

        // A dispatch error never even starts the watch.
        let never: Result<std::future::Pending<ReloadWatchResult>, String> =
            Err("main window 'main' not found".to_string());
        assert_eq!(
            run_reload_rung(never).await,
            Err(ReloadEscalation::DispatchFailed {
                detail: "main window 'main' not found".to_string()
            })
        );

        // UNKNOWN is not a failure: no readable pong stamp (no managed
        // `AppState`) reports the reload done rather than escalating into a
        // destroy/rebuild on the strength of an absence.
        assert_eq!(
            run_reload_rung(Ok(async { settled(RungPongVerdict::Unverifiable) })).await,
            Ok(RecoveryOutcome::Reloaded { verified: false })
        );
    }

    /// Round 1 hardened the reload watch in two places that NOTHING could
    /// reach: the re-baseline onto WebView2's acceptance instant, and the
    /// post-settle acceptance read. Both lived behind an `&tauri::AppHandle`,
    /// which no unit test can build — so deleting either (or the whole reload
    /// block in `trigger_ui_recovery`) left the suite green. `last_pong` is
    /// now the stamp itself and `document_now` the identity reader, which is
    /// what makes these tests possible.
    ///
    /// Here: the watch must re-baseline onto the ACCEPTANCE instant. A pong
    /// stamped between the dispatch and the acceptance predates the navigation
    /// WebView2 actually started — `with_webview` only enqueues — so it must
    /// not credit the rung even when it carries a new document identity.
    #[tokio::test]
    async fn the_watch_re_baselines_onto_the_acceptance_instant() {
        const DISPATCHED_MS: u64 = 10_000;
        const ACCEPTED_MS: u64 = DISPATCHED_MS + 10;
        let last_pong = std::sync::Arc::new(AtomicU64::new(0));
        let pong = last_pong.clone();
        let mut ticks = 0u32;

        let watch = watch_for_main_pong(
            Some(last_pong),
            DISPATCHED_MS,
            Some("doc-before".to_string()),
            // One poll interval is all this needs: the watch measures its
            // deadline from its own start, so it settles on the second tick.
            1,
            // A DIFFERENT document is ponging — the timestamp is the only
            // thing standing between this and a false `Live`.
            || Some("doc-after".to_string()),
            || {
                ticks += 1;
                if ticks == 1 {
                    // Stamped after the dispatch, BEFORE the acceptance.
                    pong.store(DISPATCHED_MS + 1, Ordering::Relaxed);
                    RungTick::Rebaseline {
                        rung_done_ms: ACCEPTED_MS,
                    }
                } else {
                    RungTick::Continue
                }
            },
        )
        .await;

        assert_eq!(
            watch,
            RungWatch::Settled(RungPongVerdict::NoPong),
            "a pong predating WebView2's acceptance must not credit the reload \
             — delete the re-baseline and this reads Live"
        );
    }

    /// The second conjunct, through the watch rather than through
    /// `classify_rung_pong` directly: after the re-baseline, a pong stamped
    /// one millisecond after the acceptance but carrying the SAME document is
    /// the pre-reload page still answering pings. It must not settle `Live`.
    #[tokio::test]
    async fn an_unchanged_document_never_settles_live_after_the_rebaseline() {
        const DISPATCHED_MS: u64 = 10_000;
        const ACCEPTED_MS: u64 = DISPATCHED_MS + 10;
        let last_pong = std::sync::Arc::new(AtomicU64::new(0));
        let pong = last_pong.clone();
        let mut ticks = 0u32;

        let watch = watch_for_main_pong(
            Some(last_pong),
            DISPATCHED_MS,
            Some("doc-before".to_string()),
            1,
            // The page the reload was meant to replace, still ponging.
            || Some("doc-before".to_string()),
            || {
                ticks += 1;
                if ticks == 1 {
                    pong.store(ACCEPTED_MS + 1, Ordering::Relaxed);
                    RungTick::Rebaseline {
                        rung_done_ms: ACCEPTED_MS,
                    }
                } else {
                    RungTick::Continue
                }
            },
        )
        .await;

        assert_eq!(
            watch,
            RungWatch::Settled(RungPongVerdict::NoPong),
            "the 2026-09-18 shape: a fresh timestamp from the OLD document is \
             not evidence the reload took"
        );

        // …and the same watch with a changed identity does settle `Live`, so
        // the assertion above is about the identity and not about the clock.
        assert_eq!(
            watch_for_main_pong(
                Some(std::sync::Arc::new(AtomicU64::new(ACCEPTED_MS + 1))),
                ACCEPTED_MS,
                Some("doc-before".to_string()),
                1,
                || Some("doc-after".to_string()),
                || RungTick::Continue,
            )
            .await,
            RungWatch::Settled(RungPongVerdict::Live)
        );
    }

    /// The post-settle acceptance read in `verify_reload_took`. A refusal that
    /// arrives after the pong watch has already settled is still WebView2
    /// saying "I never ran your call", so it must reach
    /// `ReloadWatchResult.acceptance` and escalate.
    ///
    /// The watch settles `Live` on its FIRST classify here, so `signal` — and
    /// with it `AcceptanceWatch::poll` — is never called from inside the loop:
    /// only the read after the loop can find the refusal.
    #[tokio::test]
    async fn a_refusal_arriving_after_the_watch_settles_still_reaches_the_result() {
        const DISPATCHED_MS: u64 = 10_000;
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Err("Reload(): E_FAIL".to_string())).unwrap();

        let result = verify_reload_took(
            Some(std::sync::Arc::new(AtomicU64::new(DISPATCHED_MS + 1))),
            DISPATCHED_MS,
            Some("doc-before".to_string()),
            || Some("doc-after".to_string()),
            ReloadDispatch { accepted: Some(rx) },
        )
        .await;

        assert_eq!(
            result.watch,
            RungWatch::Settled(RungPongVerdict::Live),
            "the pong settles first — that is the whole point of this case"
        );
        assert_eq!(
            result.acceptance,
            ReloadAcceptance::Refused {
                detail: "Reload(): E_FAIL".to_string()
            },
            "delete the post-settle acceptance poll and this reads Unanswered"
        );
        assert_eq!(
            run_reload_rung(Ok(async move { result })).await,
            Err(ReloadEscalation::Refused {
                detail: "Reload(): E_FAIL".to_string()
            }),
            "WebView2's refusal outranks a pong that arrived first"
        );

        // No managed `AppState` ⇒ no stamp to read ⇒ UNKNOWN, not a failure,
        // and the acceptance still latches through the same post-settle read.
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Ok(())).unwrap();
        let unverifiable = verify_reload_took(
            None,
            DISPATCHED_MS,
            None,
            || None,
            ReloadDispatch { accepted: Some(rx) },
        )
        .await;
        assert_eq!(
            unverifiable.watch,
            RungWatch::Settled(RungPongVerdict::Unverifiable)
        );
        assert!(matches!(
            unverifiable.acceptance,
            ReloadAcceptance::Accepted { .. }
        ));
    }

    /// `NoPong` carries WebView2's own answer, because "accepted, then nothing
    /// ponged" and "never answered at all" are different incidents — and
    /// `describe()` puts this on an operator-visible line. The variant used to
    /// say "reload accepted" for both, and to name a deadline it measured from
    /// the dispatch rather than from the watch.
    #[tokio::test]
    async fn no_pong_reports_whether_webview2_ever_answered() {
        async fn no_pong(
            acceptance: ReloadAcceptance,
        ) -> Result<RecoveryOutcome, ReloadEscalation> {
            run_reload_rung(Ok(async move {
                ReloadWatchResult {
                    watch: RungWatch::Settled(RungPongVerdict::NoPong),
                    acceptance,
                }
            }))
            .await
        }

        let accepted = no_pong(ReloadAcceptance::Accepted { at_ms: 1 }).await;
        assert_eq!(
            accepted,
            Err(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted: true
            })
        );
        let line = accepted.unwrap_err().describe();
        assert!(line.contains("reload accepted"), "{line}");
        assert!(line.contains("of the watch"), "{line}");

        // Unanswered is UNKNOWN: the escalation must not claim acceptance.
        let unanswered = no_pong(ReloadAcceptance::Unanswered).await;
        assert_eq!(
            unanswered,
            Err(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted: false
            })
        );
        let line = unanswered.unwrap_err().describe();
        assert!(
            !line.contains("accepted"),
            "an unanswered reload must not be described as accepted: {line}"
        );
        assert!(line.contains("never answered"), "{line}");
    }

    /// Finding 2: a refusal that lands AFTER the watch settled `Live` used to
    /// die with the receiver. It is WebView2 saying the call never ran, so it
    /// outranks the pong and escalates.
    #[tokio::test]
    async fn a_refusal_latched_after_a_live_verdict_still_escalates() {
        assert_eq!(
            run_reload_rung(Ok(async {
                ReloadWatchResult {
                    watch: RungWatch::Settled(RungPongVerdict::Live),
                    acceptance: ReloadAcceptance::Refused {
                        detail: "CoreWebView2(): E_POINTER".to_string(),
                    },
                }
            }))
            .await,
            Err(ReloadEscalation::Refused {
                detail: "CoreWebView2(): E_POINTER".to_string()
            }),
            "a late refusal must not be swallowed by a pong that arrived first"
        );
        // An UNANSWERED acceptance is not a refusal, and must not escalate a
        // reload the pong verified.
        assert_eq!(
            run_reload_rung(Ok(async {
                ReloadWatchResult {
                    watch: RungWatch::Settled(RungPongVerdict::Live),
                    acceptance: ReloadAcceptance::Unanswered,
                }
            }))
            .await,
            Ok(RecoveryOutcome::Reloaded { verified: true })
        );
    }

    #[tokio::test]
    async fn reload_dispatch_acceptance_is_waited_for_and_classified() {
        let accepted = |r: Option<Result<(), String>>| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if let Some(r) = r {
                tx.send(r).unwrap();
            } else {
                drop(tx);
            }
            ReloadDispatch { accepted: Some(rx) }
        };
        let t = std::time::Duration::from_millis(50);
        assert_eq!(
            accepted(Some(Ok(()))).accepted_within(t).await,
            Ok(Some(()))
        );
        assert_eq!(
            accepted(Some(Err("nope".to_string())))
                .accepted_within(t)
                .await,
            Err("nope".to_string())
        );
        assert_eq!(
            accepted(None).accepted_within(t).await,
            Err(RELOAD_DROPPED_UNRUN.to_string())
        );
        // No answer inside the timeout is UNKNOWN, not a refusal.
        let (_tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        assert_eq!(
            ReloadDispatch { accepted: Some(rx) }
                .accepted_within(t)
                .await,
            Ok(None)
        );
        // The eval fallback has no answer to wait for.
        assert_eq!(
            ReloadDispatch { accepted: None }.accepted_within(t).await,
            Ok(None)
        );
    }

    #[test]
    fn the_outcome_distinguishes_a_verified_reload_from_an_escalated_one() {
        // "reload dispatched and UI ponged"
        let reloaded = serde_json::to_value(RecoveryOutcome::Reloaded { verified: true }).unwrap();
        assert_eq!(reloaded["outcome"], "reloaded");
        assert_eq!(reloaded["verified"], true);

        // "reload dispatched, no pong, escalated" — the recreate's outcome
        // names the reload that did not take.
        let escalated = serde_json::to_value(RecoveryOutcome::Recreated {
            escalated_from_reload: Some(ReloadEscalation::NoPong {
                deadline_ms: RELOAD_PONG_DEADLINE_MS,
                accepted: true,
            }),
        })
        .unwrap();
        assert_eq!(escalated["outcome"], "recreated");
        assert_eq!(escalated["escalated_from_reload"]["why"], "no_pong");
        assert_eq!(
            escalated["escalated_from_reload"]["deadline_ms"],
            RELOAD_PONG_DEADLINE_MS
        );

        // A run that PLANNED recreate says so with an explicit null, not an
        // absent key a reader could mistake for an older build.
        let planned = serde_json::to_value(RecoveryOutcome::Recreated {
            escalated_from_reload: None,
        })
        .unwrap();
        assert!(planned["escalated_from_reload"].is_null());
        assert!(planned
            .as_object()
            .unwrap()
            .contains_key("escalated_from_reload"));

        // Every escalation reason has its own stable tag, and a failed
        // recreate after a refused reload carries both facts.
        let failed = serde_json::to_value(RecoveryOutcome::Failed {
            detail: "rebuilt blank".to_string(),
            escalated_from_reload: Some(ReloadEscalation::Refused {
                detail: "Reload(): E_FAIL".to_string(),
            }),
        })
        .unwrap();
        assert_eq!(failed["outcome"], "failed");
        assert_eq!(failed["escalated_from_reload"]["why"], "refused");
        let dispatch = serde_json::to_value(ReloadEscalation::DispatchFailed {
            detail: "main window 'main' not found".to_string(),
        })
        .unwrap();
        assert_eq!(dispatch["why"], "dispatch_failed");
    }

    #[test]
    fn recover_ui_response_flattens_the_escalation_onto_the_wire() {
        // `/ui/recover` is where an operator reads it; the flatten must carry
        // the new fields rather than swallowing them.
        let resp = RecoverUiResponse {
            reason: RecoveryReason::Manual.as_str(),
            result: RecoveryOutcome::Recreated {
                escalated_from_reload: Some(ReloadEscalation::NoPong {
                    deadline_ms: 7,
                    accepted: true,
                }),
            },
            attempts: 1,
            exhausted: false,
            server_mode: false,
            ui_recovery: classify_latch(None),
            window_swap: classify_latch(None),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["outcome"], "recreated");
        assert_eq!(json["escalated_from_reload"]["why"], "no_pong");
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
                RecoveryReason::RendererMemoryPressure,
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
            RecoveryOutcome::Reloaded { verified: true },
            RecoveryOutcome::Recreated {
                escalated_from_reload: None,
            },
            RecoveryOutcome::Exhausted { attempts: 3 },
            RecoveryOutcome::Wedged { in_flight_ms: 1 },
            RecoveryOutcome::Failed {
                detail: "x".to_string(),
                escalated_from_reload: None,
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
        //
        // The worst healthy run starts on the reload rung, sits out the whole
        // reload pong watch, escalates IN THE SAME CALL, and then pays every
        // recreate cost — so the reload deadline is a term of its own, not
        // absorbed into any other.
        let bounded = RECOVERY_BACKOFF_MAX_MS      // longest single-run backoff
            + RELOAD_PONG_DEADLINE_MS              // post-reload pong watch, then escalate
            + WINDOW_LABEL_RELEASE_TIMEOUT_MS      // label-release poll
            + RECREATE_PONG_DEADLINE_MS; // post-recreate pong watch
        assert!(
            RECOVERY_WEDGE_AFTER_MS > bounded,
            "wedge threshold {RECOVERY_WEDGE_AFTER_MS}ms must exceed every bounded cost of a \
             healthy run ({bounded}ms), or a slow-but-healthy recreate reports as wedged"
        );
        // …and the allowance over that is the cold-profile build, which is
        // deliberately unbounded. It is one `COLD_BUNDLE_BOOT_ALLOWANCE_MS`.
        assert_eq!(
            RECOVERY_WEDGE_AFTER_MS - bounded,
            COLD_BUNDLE_BOOT_ALLOWANCE_MS,
            "the build allowance must stay derived from COLD_BUNDLE_BOOT_ALLOWANCE_MS"
        );

        // The upper end: the loop guard must not declare a FRESH incident
        // before the wedge inside it was ever reported.
        assert!(
            RECOVERY_WEDGE_AFTER_MS < RECOVERY_ATTEMPT_RESET_MS,
            "wedge threshold {RECOVERY_WEDGE_AFTER_MS}ms must stay under the incident reset \
             window {RECOVERY_ATTEMPT_RESET_MS}ms"
        );

        // The recreate pong deadline is borrowed from the UI-liveness
        // calibration, not invented here.
        assert_eq!(
            RECREATE_PONG_DEADLINE_MS,
            crate::ui_error::UI_STALE_AFTER_MS
        );
        assert!(RECREATE_PONG_DEADLINE_MS < crate::ui_error::UI_DEAD_AFTER_MS);

        // The RELOAD watch is derived from the cold-profile allowance instead
        // (see the constant): a reload is a cold bundle boot, and its timeout
        // costs a full destroy/rebuild, so the running-frontend staleness
        // number is the wrong calibration for it. It must still be at least as
        // generous as the recreate's.
        //
        // It is `COLD_BUNDLE_BOOT_ALLOWANCE_MS` and NOT `RECOVERY_BACKOFF_MAX_MS`
        // on purpose: the backoff answers "how long may the loop guard sleep
        // between attempts?", which is a different question that merely had
        // the same answer. Naming the shared meaning is what lets one move
        // without silently retuning the other.
        assert_eq!(RELOAD_PONG_DEADLINE_MS, COLD_BUNDLE_BOOT_ALLOWANCE_MS);
        assert!(RELOAD_PONG_DEADLINE_MS >= RECREATE_PONG_DEADLINE_MS);
        // The whole ladder still has to fit inside the incident reset window
        // — the cold-boot allowance is a term of it twice over.
        assert!(2 * COLD_BUNDLE_BOOT_ALLOWANCE_MS < RECOVERY_ATTEMPT_RESET_MS);

        // It stays under the dead threshold — but NOT for the reason this
        // comment used to give. The heartbeat backstop fires on the age of the
        // MAIN-window pong, which a reload dispatch does not touch, so it can
        // become due at any point during this watch; what makes that harmless
        // is the single-flight latch, which refuses the backstop's trigger as
        // `already_in_progress` while this run holds it (and reports `Wedged`
        // only past RECOVERY_WEDGE_AFTER_MS, far beyond either deadline).
        // The inequality is kept because a reload watch outliving
        // UI_DEAD_AFTER_MS would mean the runner reads its UI as DEAD while
        // the ladder is still calling the rung unverified — a window in which
        // every external trigger is refused and nothing says why.
        assert!(RELOAD_PONG_DEADLINE_MS < crate::ui_error::UI_DEAD_AFTER_MS);
    }

    // ── Phase 2: did the recreate actually produce a live UI? ───────────

    /// A recreate that finished at `T0`.
    const RECREATE_DONE: u64 = T0;

    #[test]
    fn a_pong_after_the_recreate_proves_the_rebuilt_ui_is_live() {
        assert_eq!(
            classify_rung_pong(
                RECREATE_DONE + 1,
                RECREATE_DONE,
                true,
                0,
                RECREATE_PONG_DEADLINE_MS
            ),
            RungPongVerdict::Live
        );
        assert_eq!(
            classify_rung_pong(
                RECREATE_DONE + 4_000,
                RECREATE_DONE,
                true,
                4_100,
                RECREATE_PONG_DEADLINE_MS
            ),
            RungPongVerdict::Live
        );
    }

    #[test]
    fn a_pong_from_before_the_recreate_proves_nothing() {
        // The bug this rung closes: the window that ponged is the one that was
        // just destroyed. Strictly-after, so even a same-millisecond stamp is
        // not credited.
        for stale in [0, 1, RECREATE_DONE - 1, RECREATE_DONE] {
            assert_eq!(
                classify_rung_pong(stale, RECREATE_DONE, true, 0, RECREATE_PONG_DEADLINE_MS),
                RungPongVerdict::Waiting,
                "last_pong {stale} must not count as proof of the rebuilt window"
            );
            assert_eq!(
                classify_rung_pong(
                    stale,
                    RECREATE_DONE,
                    true,
                    RECREATE_PONG_DEADLINE_MS,
                    RECREATE_PONG_DEADLINE_MS
                ),
                RungPongVerdict::NoPong,
                "last_pong {stale} at the deadline is a failed recreate"
            );
        }
    }

    #[test]
    fn the_recreate_watch_waits_out_its_deadline_before_failing() {
        for elapsed in [0, 1, RECREATE_PONG_DEADLINE_MS - 1] {
            assert_eq!(
                classify_rung_pong(0, RECREATE_DONE, true, elapsed, RECREATE_PONG_DEADLINE_MS),
                RungPongVerdict::Waiting,
                "elapsed {elapsed}"
            );
        }
        for elapsed in [
            RECREATE_PONG_DEADLINE_MS,
            RECREATE_PONG_DEADLINE_MS + 1,
            u64::MAX,
        ] {
            assert_eq!(
                classify_rung_pong(0, RECREATE_DONE, true, elapsed, RECREATE_PONG_DEADLINE_MS),
                RungPongVerdict::NoPong,
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
            classify_rung_pong(0, RECREATE_DONE, true, 0, RECREATE_PONG_DEADLINE_MS),
            RungPongVerdict::Waiting
        );
        assert_eq!(
            classify_rung_pong(
                0,
                RECREATE_DONE,
                true,
                RECREATE_PONG_DEADLINE_MS,
                RECREATE_PONG_DEADLINE_MS
            ),
            RungPongVerdict::NoPong
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
            escalated_from_reload: None,
        };
        assert_ne!(
            failed.as_str(),
            RecoveryOutcome::Recreated {
                escalated_from_reload: None
            }
            .as_str()
        );
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
