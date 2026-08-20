//! Process-global cache of the runner's OWN main-window `HWND`, so a detector
//! can name the window it wants to probe without ever touching the Tauri event
//! loop.
//!
//! Plan `2026-08-19-runner-blocked-ui-thread-cannot-be-closed`, Phase 4 step 1.
//!
//! # Why this module exists at all
//!
//! `tauri::Window::hwnd()` looks like a cheap accessor and is not one. It is an
//! **unbounded event-loop getter**: `tauri-2.11.1/src/window/mod.rs:1656` →
//! `get_raw_window_handle` (`tauri-runtime-wry-2.11.2/src/lib.rs:1971`) →
//! `window_getter!` → `getter!` (`:197-204`), whose last act is `rx.recv()`
//! **with no timeout**. Calling it from a native-hang detector parks the
//! detector on precisely the thread it was written to observe — the detector
//! then shares fate with its subject and reports nothing, forever.
//!
//! Calling it **once, at startup, on the UI thread** is safe: `send_user_message`
//! short-circuits to a direct inline call when it is already on the main thread
//! (`tauri-runtime-wry-2.11.2/src/lib.rs:239-247`), so there is no channel and
//! no `recv()`. That is the only place [`set_main_hwnd`] should be called from.
//!
//! # Crate choice is load-bearing
//!
//! Three `windows`-family crates are in this build at three versions
//! (`windows-sys 0.59`, `windows 0.58`, and `windows 0.61` renamed to
//! `windows-capture`), and `HWND` is a *distinct type* in each. This module
//! stays entirely inside **`windows-sys 0.59`** and hands out a plain `isize`,
//! so a consumer can cast to whichever `HWND` its own call site needs without
//! this module having an opinion. `isize` is also `Send`, which the raw
//! `*mut c_void` `HWND` is not — that is what lets the handle live in a
//! process-global atomic readable from a monitor thread.

/// The cached main-window handle, as a raw `isize`.
///
/// `0` is the sentinel for "not resolved yet" — a real `HWND` is never null.
/// An atomic (rather than a `OnceLock`) because the fallback resolver may fill
/// it in later, from a different thread, without the startup path having run.
#[cfg(windows)]
static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Warn once — not on every 5 s tick — the first time the cache is found stale.
#[cfg(windows)]
static STALE_CACHE_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record the main window's `HWND`.
///
/// **Call this exactly once, from the UI thread, right after the main window is
/// built** — never from a detector (see the module docs). A zero value is
/// ignored: it is the "unresolved" sentinel, so caching it would be
/// indistinguishable from never having run.
#[cfg(windows)]
pub fn set_main_hwnd(hwnd: isize) {
    if hwnd == 0 {
        tracing::warn!("ui_thread_probe: refusing to cache a null main-window HWND");
        return;
    }
    MAIN_HWND.store(hwnd, std::sync::atomic::Ordering::SeqCst);
    tracing::info!("ui_thread_probe: cached main-window HWND {hwnd:#x}");
}

/// Forget the cached handle, so the next [`main_hwnd`] re-resolves from
/// scratch.
///
/// **Call this from any path that destroys the main window** — today the one
/// caller is `webview_recovery::recreate_main_window`, which does
/// `existing.destroy()` and rebuilds. Without it the memo would keep handing
/// out the handle of a window that no longer exists.
///
/// This is belt-and-braces, not the only defence: [`main_hwnd`] validates the
/// cache with `IsWindow` on every read, so a missed invalidation costs one
/// extra `EnumWindows` sweep rather than permanently disabling detection. Both
/// exist because the failure mode is silent and terminal — a stale memo made
/// the Phase 4 probe report UNKNOWN for the **rest of the process's life**,
/// and UNKNOWN is deliberately never escalated.
#[cfg(windows)]
pub fn invalidate_main_hwnd() {
    let previous = MAIN_HWND.swap(0, std::sync::atomic::Ordering::SeqCst);
    if previous != 0 {
        tracing::info!(
            "ui_thread_probe: invalidated cached main-window HWND {previous:#x} \
             (window destroyed) — the next probe will re-resolve"
        );
    }
    // A fresh window is a fresh reason to be told about staleness.
    STALE_CACHE_LOGGED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// The main window's `HWND` as a raw `isize`, or `None` if it cannot be
/// established.
///
/// Resolution order:
/// 1. the value cached by [`set_main_hwnd`] at startup, **validated with
///    `IsWindow`** (the normal path);
/// 2. an `EnumWindows` + `GetWindowThreadProcessId` sweep over **this process's
///    own** top-level windows, for the case where the startup cache did not run
///    or no longer names a live window (headless/server mode, a window rebuilt
///    by webview recovery, a detector that starts before window construction).
///    A successful sweep is memoized into the same slot so the 5 s-cadence
///    caller does not re-enumerate.
///
/// Never touches the Tauri event loop on either path.
///
/// # Why the cache is validated rather than trusted
///
/// `set_main_hwnd` is called exactly once, from `main.rs`'s window-construction
/// arm. `webview_recovery::recreate_main_window` then `destroy()`s that window
/// and builds a new one — after which the memo names a dead handle,
/// `SendMessageTimeoutW` fails with `ERROR_INVALID_WINDOW_HANDLE`, and the
/// Phase 4 probe reports UNKNOWN. UNKNOWN is (correctly) never escalated, so
/// without this validation **one webview recovery turns native-hang detection
/// off for the remaining lifetime of the process** — the exact class of silent,
/// permanent blindness this whole plan exists to remove.
///
/// `IsWindow` is cheap and non-blocking (it inspects the window table; it does
/// not message the owning thread), so it is safe to call from the detector at
/// its 5 s cadence and cannot inherit the hang. Its one documented weakness —
/// a recycled handle value can make a dead handle look live — is covered by the
/// sweep's own-PID filter on the re-resolve path, and is in any case a strictly
/// better failure than the unconditional staleness it replaces.
///
/// `allow(dead_code)`: the periodic reader is the native-message-loop liveness
/// rung in `health_monitor`; the HTTP reader is the close-door verdict in
/// `mcp/ui_bridge/page.rs`. Both are `cfg`-independent callers, so this stays
/// annotated for builds that compile neither.
#[allow(dead_code)]
#[cfg(windows)]
pub fn main_hwnd() -> Option<isize> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

    let cached = MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst);
    if cached != 0 {
        // SAFETY: a read-only query against the OS window table. A stale or
        // bogus handle is exactly what this call is for — it answers `FALSE`
        // rather than faulting.
        if unsafe { IsWindow(cached as HWND) } != 0 {
            return Some(cached);
        }
        // Stale: the window was destroyed (a `webview_recovery` recreate, most
        // likely) without anyone invalidating the memo. Drop it and re-resolve.
        MAIN_HWND.store(0, std::sync::atomic::Ordering::SeqCst);
        if !STALE_CACHE_LOGGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!(
                "ui_thread_probe: cached main-window HWND {cached:#x} no longer names a live \
                 window — re-resolving via EnumWindows"
            );
        }
    }
    let found = resolve_own_main_window()?;
    // Memoize the fallback so a periodic caller pays the sweep once. Racing
    // writers necessarily agree (same process, same window), so a plain store
    // is fine.
    MAIN_HWND.store(found, std::sync::atomic::Ordering::SeqCst);
    STALE_CACHE_LOGGED.store(false, std::sync::atomic::Ordering::SeqCst);
    tracing::info!("ui_thread_probe: resolved main-window HWND {found:#x} via EnumWindows");
    Some(found)
}

/// The title the main window is built with
/// (`webview_recovery::build_main_window`, `.title("Qontinui Runner")`). Used
/// only to *prefer* the main window over a pop-out terminal window in the
/// fallback sweep; a title mismatch degrades to "first visible titled window of
/// this process", it does not fail the resolution.
#[cfg(windows)]
const MAIN_WINDOW_TITLE: &str = "Qontinui Runner";

/// Sweep this process's own top-level windows and pick the main one.
///
/// Mirrors the `EnumWindows` + `GetWindowThreadProcessId` pattern already used
/// at `window_manager.rs:132`/`:188` — with the one difference that those
/// enumerate *other* processes' windows and this one deliberately matches only
/// `GetCurrentProcessId()`.
///
/// `allow(dead_code)`: reachable only through [`main_hwnd`]; see the note there.
#[allow(dead_code)]
#[cfg(windows)]
fn resolve_own_main_window() -> Option<isize> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    /// `(exact-title match, first visible titled window, our pid)`.
    struct Sweep {
        exact: Option<isize>,
        first: Option<isize>,
        pid: u32,
    }

    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `&mut Sweep` handed to `EnumWindows` below,
        // which outlives the (synchronous) enumeration.
        let sweep = unsafe { &mut *(lparam as *mut Sweep) };

        let mut wnd_pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut wnd_pid) };
        if wnd_pid != sweep.pid {
            return TRUE;
        }
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return TRUE;
        }
        let title_len = unsafe { GetWindowTextLengthW(hwnd) };
        if title_len <= 0 {
            // Untitled top-level windows of our own process are wry/WebView2
            // internals, never the main window.
            return TRUE;
        }
        let mut buf = vec![0u16; (title_len + 1) as usize];
        let written = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let title = std::ffi::OsString::from_wide(&buf[..written.max(0) as usize])
            .to_string_lossy()
            .into_owned();

        let handle = hwnd as isize;
        if sweep.first.is_none() {
            sweep.first = Some(handle);
        }
        if title == MAIN_WINDOW_TITLE {
            sweep.exact = Some(handle);
            return 0; // stop enumerating — this is the one we wanted
        }
        TRUE
    }

    let mut sweep = Sweep {
        exact: None,
        first: None,
        pid: std::process::id(),
    };
    // SAFETY: `enum_callback` only dereferences `lparam` as `&mut Sweep`, and
    // `sweep` is alive for the whole synchronous call.
    unsafe {
        EnumWindows(
            Some(enum_callback),
            &mut sweep as *mut Sweep as LPARAM,
        );
    }
    sweep.exact.or(sweep.first)
}

// ── Non-Windows stub ────────────────────────────────────────────────────────
//
// There is no portable equivalent of an HWND, and the native-message-loop
// liveness rung this cache exists to serve is Windows-only by construction
// (`SendMessageTimeoutW`). On other platforms the already-shipped,
// already-cross-platform arm remains: heartbeat staleness at 90 s
// (`heartbeat.rs` → `ui_error.rs`). The stub keeps every call site
// `cfg`-free — the detector simply gets `None` and stands down.

/// No-op on non-Windows: there is no HWND to cache.
#[cfg(not(windows))]
pub fn set_main_hwnd(_hwnd: isize) {}

/// No-op on non-Windows: there is no HWND to invalidate.
#[cfg(not(windows))]
pub fn invalidate_main_hwnd() {}

/// Always `None` on non-Windows: there is no HWND to resolve.
#[allow(dead_code)]
#[cfg(not(windows))]
pub fn main_hwnd() -> Option<isize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every invariant of the cache slot, asserted in ONE test on purpose:
    /// `MAIN_HWND` is a process-global static, so two `#[test]` fns touching it
    /// would race under the default parallel test runner.
    ///
    /// 1. The null sentinel is never cached — storing it would make "cached a
    ///    bogus handle" indistinguishable from "startup never ran", and the
    ///    `EnumWindows` fallback would then be skipped forever.
    /// 2. A cached handle is **validated, not trusted**: `0x4242` is not a live
    ///    window, so `main_hwnd()` must NOT hand it back. This is the whole
    ///    handback fix — before it, a `webview_recovery` recreate left a dead
    ///    handle memoized and the native-hang probe reported UNKNOWN for the
    ///    rest of the process's life.
    /// 3. A failed validation clears the slot, so the next call re-resolves
    ///    instead of re-validating a handle already known to be dead.
    /// 4. [`invalidate_main_hwnd`] clears the slot outright — the explicit door
    ///    `webview_recovery` calls when it destroys the window.
    #[cfg(windows)]
    #[test]
    fn cache_slot_rejects_null_validates_and_can_be_invalidated() {
        let previous = MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst);

        // (1) the null sentinel is refused
        MAIN_HWND.store(0, std::sync::atomic::Ordering::SeqCst);
        set_main_hwnd(0);
        assert_eq!(
            MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a null HWND must not be stored"
        );

        // (2) a stored-but-dead handle is never handed out
        set_main_hwnd(0x4242);
        assert_eq!(
            MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst),
            0x4242,
            "set_main_hwnd should store a non-null handle"
        );
        assert_ne!(
            main_hwnd(),
            Some(0x4242),
            "a cached handle that is not a live window must not be returned — that is \
             exactly the stale-memo bug this validation closes"
        );

        // (3) the failed validation cleared the slot (the EnumWindows sweep in
        // a headless test process finds nothing to memoize in its place)
        assert_ne!(
            MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst),
            0x4242,
            "a handle that failed IsWindow must not stay cached"
        );

        // (4) the explicit invalidation door empties the slot
        set_main_hwnd(0x4343);
        invalidate_main_hwnd();
        assert_eq!(
            MAIN_HWND.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "invalidate_main_hwnd must clear the cache"
        );

        MAIN_HWND.store(previous, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_stub_is_always_none() {
        set_main_hwnd(0x4242);
        invalidate_main_hwnd();
        assert_eq!(main_hwnd(), None);
    }
}
