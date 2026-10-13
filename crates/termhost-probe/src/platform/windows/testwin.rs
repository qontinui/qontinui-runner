//! The probe's own window, painted with GDI: the selftest's measured window
//! and the doctor's capture probe.
//!
//! Markers are painted as crisp cells with exact palette colours from any
//! thread (GetDC + FillRect + GdiFlush), so a scheduled paint lands within a
//! fraction of a millisecond of its time; WM_PAINT repaints the current state
//! after the window is uncovered. Every WM_CHAR is answered with an echo marker
//! (kind echo, seq = keys received so far), the selftest's key round trip. A
//! second, opaque window of ours covers it on request (the occlusion proof),
//! and the window's thread can be told to stop pumping messages (the OS
//! "not responding" proof).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GdiFlush, GetDC, ReleaseDC,
    UpdateWindow, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageTimeoutW,
    SetWindowPos, ShowWindow, TranslateMessage, HWND_TOP, HWND_TOPMOST, IDC_ARROW, MSG,
    SET_WINDOW_POS_FLAGS, SMTO_ABORTIFHUNG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_CLOSE, WM_DESTROY,
    WM_ERASEBKGND, WM_PAINT, WNDCLASSEXW, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU,
};

use super::{handle_of, window};
use crate::clock::now_ns;
use crate::codec::{encode_values, marker_seq, Marker, MarkerKind, CELLS, PALETTE};
use crate::platform::{
    EchoPainted, PaintSlot, TestGeometry, TestWindow, TestWindowOptions, WindowHandle,
};

const WM_APP_FREEZE: u32 = WM_APP + 1;
const WM_APP_ACTIVATE: u32 = WM_APP + 2;
const WM_APP_OCCLUDE: u32 = WM_APP + 3;
const PAD: u32 = 8;

struct Shared {
    geometry: TestGeometry,
    echo_session: u16,
    state: Mutex<[Option<Marker>; 2]>,
    echoes: Mutex<Vec<EchoPainted>>,
    brushes: [isize; 8],
    occluder_brush: isize,
    main: AtomicIsize,
    occluder: AtomicIsize,
}

impl Shared {
    fn slot_origin(&self, slot: PaintSlot) -> (i32, i32) {
        let (x, y) = match slot {
            PaintSlot::Output => self.geometry.output_at,
            PaintSlot::Echo => self.geometry.echo_at,
        };
        (x as i32, y as i32)
    }

    /// Paint one slot into `hdc`.
    fn paint_slot(&self, hdc: HDC, slot: PaintSlot, marker: Option<Marker>) {
        let (x, y) = self.slot_origin(slot);
        let (cw, ch) = (self.geometry.cell_w as i32, self.geometry.cell_h as i32);
        let values = marker.map(encode_values);
        for i in 0..CELLS as i32 {
            let r = RECT {
                left: x + i * cw,
                top: y,
                right: x + (i + 1) * cw,
                bottom: y + ch,
            };
            let brush = match values {
                Some(v) => self.brushes[usize::from(v[i as usize])],
                None => self.brushes[0],
            };
            // SAFETY: a DC of our window and a brush we own.
            unsafe { FillRect(hdc, &r, HBRUSH(brush as *mut core::ffi::c_void)) };
        }
    }

    fn main(&self) -> HWND {
        HWND(self.main.load(Ordering::Relaxed) as *mut core::ffi::c_void)
    }

    fn occluder(&self) -> HWND {
        HWND(self.occluder.load(Ordering::Relaxed) as *mut core::ffi::c_void)
    }

    /// Paint from any thread; returns the probe-clock time after the flush.
    fn paint_now(&self, slot: PaintSlot, marker: Option<Marker>) -> Result<u64, String> {
        {
            let mut st = self.state.lock().map_err(|_| "paint state poisoned")?;
            st[match slot {
                PaintSlot::Output => 0,
                PaintSlot::Echo => 1,
            }] = marker;
        }
        let h = self.main();
        // SAFETY: a DC of our own window, released below.
        unsafe {
            let hdc = GetDC(Some(h));
            if hdc.is_invalid() {
                return Err("GetDC failed".into());
            }
            self.paint_slot(hdc, slot, marker);
            let _ = GdiFlush();
            ReleaseDC(Some(h), hdc);
        }
        Ok(now_ns())
    }
}

thread_local! {
    static WINDOW: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

fn shared() -> Option<Arc<Shared>> {
    WINDOW.with(|w| w.borrow().clone())
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(s) = shared() else {
        // SAFETY: default processing.
        return unsafe { DefWindowProcW(h, msg, wparam, lparam) };
    };
    let is_main = h == s.main();
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: BeginPaint/EndPaint pair on our own window.
            unsafe {
                let hdc = BeginPaint(h, &mut ps);
                if is_main {
                    FillRect(
                        hdc,
                        &ps.rcPaint,
                        HBRUSH(s.brushes[0] as *mut core::ffi::c_void),
                    );
                    let st = s.state.lock().map(|g| *g).unwrap_or([None, None]);
                    s.paint_slot(hdc, PaintSlot::Output, st[0]);
                    s.paint_slot(hdc, PaintSlot::Echo, st[1]);
                } else {
                    FillRect(
                        hdc,
                        &ps.rcPaint,
                        HBRUSH(s.occluder_brush as *mut core::ffi::c_void),
                    );
                }
                let _ = EndPaint(h, &ps);
            }
            LRESULT(0)
        }
        WM_CHAR if is_main => {
            if let Some(ch) = char::from_u32(wparam.0 as u32).filter(|c| !c.is_control()) {
                let seq = s.echoes.lock().map(|e| e.len()).unwrap_or(0) as u32;
                let m = Marker::new(MarkerKind::Echo, s.echo_session, marker_seq(u64::from(seq)));
                if let Ok(t) = s.paint_now(PaintSlot::Echo, Some(m)) {
                    if let Ok(mut e) = s.echoes.lock() {
                        e.push(EchoPainted {
                            seq,
                            ch,
                            t_painted_ns: t,
                        });
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_FREEZE if is_main => {
            // Deliberately stop servicing messages (IsHungAppWindow after 5 s).
            std::thread::sleep(Duration::from_millis(wparam.0 as u64));
            LRESULT(0)
        }
        WM_APP_ACTIVATE if is_main => LRESULT(isize::from(window::activate(h))),
        WM_APP_OCCLUDE if is_main => {
            let occ = s.occluder();
            // SAFETY: positioning/hiding our own second window.
            unsafe {
                if wparam.0 != 0 {
                    match window::bounds(h) {
                        Some(b) => {
                            let _ = SetWindowPos(
                                occ,
                                Some(HWND_TOPMOST),
                                b.x - 10,
                                b.y - 10,
                                b.w as i32 + 20,
                                b.h as i32 + 20,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                            );
                        }
                        None => return LRESULT(0),
                    }
                } else {
                    let _ = ShowWindow(occ, SW_HIDE);
                }
            }
            LRESULT(1)
        }
        WM_CLOSE if is_main => {
            // SAFETY: destroying our own windows.
            unsafe {
                let _ = DestroyWindow(s.occluder());
                let _ = DestroyWindow(h);
            }
            LRESULT(0)
        }
        WM_DESTROY if is_main => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default processing.
        _ => unsafe { DefWindowProcW(h, msg, wparam, lparam) },
    }
}

static SEQ: AtomicU32 = AtomicU32::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct GdiTestWindow {
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

impl GdiTestWindow {
    pub fn create(opts: &TestWindowOptions) -> Result<GdiTestWindow, String> {
        let need_w = PAD * 2 + opts.cell_w * CELLS as u32;
        let need_h = PAD * 3 + opts.cell_h * 2;
        let geometry = TestGeometry {
            cell_w: opts.cell_w,
            cell_h: opts.cell_h,
            output_at: (PAD, PAD),
            echo_at: (PAD, PAD * 2 + opts.cell_h),
        };
        let client = (opts.client_w.max(need_w), opts.client_h.max(need_h));
        // SAFETY: creating GDI brushes we own (deleted in Drop).
        let brushes = PALETTE.map(|p| unsafe {
            CreateSolidBrush(COLORREF(
                u32::from(p.rgb[0]) | u32::from(p.rgb[1]) << 8 | u32::from(p.rgb[2]) << 16,
            ))
            .0 as isize
        });
        // A mid-grey: impure under every threshold, so the occluder can never
        // read as a marker.
        // SAFETY: as above.
        let occluder_brush = unsafe { CreateSolidBrush(COLORREF(0x0080_7878)).0 as isize };
        let shared = Arc::new(Shared {
            geometry,
            echo_session: opts.echo_session,
            state: Mutex::new([None, None]),
            echoes: Mutex::new(Vec::new()),
            brushes,
            occluder_brush,
            main: AtomicIsize::new(0),
            occluder: AtomicIsize::new(0),
        });
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let s2 = shared.clone();
        let opts2 = opts.clone();
        let thread = std::thread::Builder::new()
            .name("test-window".into())
            .spawn(move || window_thread(s2, opts2, client, tx))
            .map_err(|e| format!("spawning the window thread: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(_)) => Ok(GdiTestWindow {
                shared,
                thread: Mutex::new(Some(thread)),
                closed: AtomicBool::new(false),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the test window did not appear within 5 s".into()),
        }
    }

    fn send(&self, msg: u32, wparam: usize, timeout_ms: u32) -> Option<isize> {
        let mut result = 0usize;
        // SAFETY: a synchronous message to our own window, bounded by a timeout.
        let ok = unsafe {
            SendMessageTimeoutW(
                self.shared.main(),
                msg,
                WPARAM(wparam),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                timeout_ms,
                Some(&mut result),
            )
        };
        (ok.0 != 0).then_some(result as isize)
    }
}

fn window_thread(
    shared: Arc<Shared>,
    opts: TestWindowOptions,
    client: (u32, u32),
    ready: mpsc::Sender<Result<u32, String>>,
) {
    let class = wide(&format!(
        "termhost-probe-window-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let title = wide(&opts.title);
    let occ_title = wide("termhost-probe occluder");
    WINDOW.with(|w| *w.borrow_mut() = Some(shared.clone()));
    // SAFETY: a window class and two windows owned by this thread; the
    // message loop runs until WM_QUIT.
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            let _ = ready.send(Err("GetModuleHandleW failed".into()));
            return;
        };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            let _ = ready.send(Err("RegisterClassExW failed".into()));
            return;
        }
        let (style, ex) = window_styles(opts.topmost);
        let mut r = RECT {
            left: 0,
            top: 0,
            right: client.0 as i32,
            bottom: client.1 as i32,
        };
        let _ = AdjustWindowRectEx(&mut r, style, false, ex);
        let main = match CreateWindowExW(
            ex,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            opts.x,
            opts.y,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(module.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("CreateWindowExW: {e}")));
                return;
            }
        };
        shared.main.store(main.0 as isize, Ordering::Relaxed);
        // The occluder: an opaque, captioned, non-activating topmost window —
        // a window the occlusion rules count (not a popup, not a tool window).
        let occ = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            PCWSTR(class.as_ptr()),
            PCWSTR(occ_title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION,
            opts.x,
            opts.y,
            100,
            100,
            None,
            None,
            Some(module.into()),
            None,
        )
        .unwrap_or_default();
        shared.occluder.store(occ.0 as isize, Ordering::Relaxed);
        show_window(main, opts.activate, opts.topmost);
        let _ = UpdateWindow(main);
        let _ = ready.send(Ok(GetCurrentThreadId()));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    WINDOW.with(|w| *w.borrow_mut() = None);
}

impl TestWindow for GdiTestWindow {
    fn handle(&self) -> WindowHandle {
        handle_of(self.shared.main())
    }

    fn geometry(&self) -> TestGeometry {
        self.shared.geometry
    }

    fn paint(&self, slot: PaintSlot, marker: Option<Marker>) -> Result<u64, String> {
        self.shared.paint_now(slot, marker)
    }

    fn set_occluded(&self, on: bool) -> Result<(), String> {
        match self.send(WM_APP_OCCLUDE, usize::from(on), 3000) {
            Some(1) => {
                std::thread::sleep(Duration::from_millis(150));
                Ok(())
            }
            other => Err(format!("the window did not (un)cover itself: {other:?}")),
        }
    }

    fn activate(&self) -> Result<bool, String> {
        self.send(WM_APP_ACTIVATE, 0, 5000)
            .map(|r| r != 0)
            .ok_or_else(|| "the window did not answer the activation request".into())
    }

    fn freeze(&self, d: Duration) -> Result<(), String> {
        let ms = d.as_millis().min(60_000) as usize;
        // SAFETY: posting to our own window.
        unsafe {
            PostMessageW(
                Some(self.shared.main()),
                WM_APP_FREEZE,
                WPARAM(ms),
                LPARAM(0),
            )
        }
        .map_err(|e| format!("PostMessageW: {e}"))?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    fn echoes(&self) -> Vec<EchoPainted> {
        self.shared
            .echoes
            .lock()
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        // SAFETY: posting to our own window.
        let _ = unsafe { PostMessageW(Some(self.shared.main()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Ok(mut t) = self.thread.lock() {
            if let Some(h) = t.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for GdiTestWindow {
    fn drop(&mut self) {
        self.close();
        for b in self
            .shared
            .brushes
            .iter()
            .chain(std::iter::once(&self.shared.occluder_brush))
        {
            // SAFETY: brushes we created, deleted once after the window is gone.
            let _ = unsafe { DeleteObject(HGDIOBJ(*b as *mut core::ffi::c_void)) };
        }
    }
}

/// The window's styles — ONE construction for the selftest's window and the
/// doctor's capture probe, so the doctor proves WGC on the same kind of
/// window the selftest then measures. Only the show command differs
/// (`show_window`): the doctor's probe, which also runs on operator
/// machines, is shown without activation and never asked to take focus.
///
/// Why it is no longer a separate, "never-activating" design: on the hosted
/// runs (PR #2177, runs 38315983341 and 38327513555) `CreateForWindow`
/// refused the doctor's probe with E_INVALIDARG (0x80070057, "Could not
/// capture the given window") — first as a `WS_POPUP` + `WS_EX_TOOLWINDOW`
/// window, then as a captioned `WS_EX_NOACTIVATE` window without
/// `WS_EX_APPWINDOW` — while it captured the selftest's ordinary
/// application window in the same runs. Which of the remaining differences
/// WGC objects to (`WS_EX_NOACTIVATE`, the missing `WS_EX_APPWINDOW` or
/// `WS_SYSMENU`) is not established; sharing the construction removes all
/// of them, and the next windows-latest run shows whether that was enough.
fn window_styles(topmost: bool) -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    let mut ex = WS_EX_APPWINDOW;
    if topmost {
        ex |= WS_EX_TOPMOST;
    }
    (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU, ex)
}

/// The flags that show the doctor's probe without activating it.
const PROBE_SHOW: SET_WINDOW_POS_FLAGS =
    SET_WINDOW_POS_FLAGS(SWP_SHOWWINDOW.0 | SWP_NOACTIVATE.0 | SWP_NOMOVE.0 | SWP_NOSIZE.0);

/// Show the window the first time. The selftest's window, which must be
/// foreground, with `ShowWindow(SW_SHOW)`. The doctor's probe — D9: it must
/// never take the operator's focus — with `SetWindowPos(SWP_SHOWWINDOW |
/// SWP_NOACTIVATE | ...)`, NOT `ShowWindow`: the docs say `nCmdShow` "is
/// ignored the first time an application calls ShowWindow, if the program
/// that launched the application provides a STARTUPINFO structure", and the
/// probe is usually that first call — a launcher passing
/// `STARTF_USESHOWWINDOW` could otherwise have it shown activated.
fn show_window(main: HWND, activate: bool, topmost: bool) {
    // SAFETY: our own window, on its own thread.
    unsafe {
        if activate {
            let _ = ShowWindow(main, SW_SHOW);
        } else {
            let after = if topmost { HWND_TOPMOST } else { HWND_TOP };
            let _ = SetWindowPos(main, Some(after), 0, 0, 0, 0, PROBE_SHOW);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doctor_and_the_selftest_build_the_same_window() {
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_TOOLWINDOW, WS_POPUP};
        // One construction: `window_styles` takes no activation parameter,
        // so the doctor's probe (never activated) and the selftest's window
        // can differ only in how they are first shown (`show_window`).
        let (style, ex) = window_styles(true);
        assert_eq!(style, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU);
        assert_eq!(ex, WS_EX_APPWINDOW | WS_EX_TOPMOST);
        assert!(!ex.contains(WS_EX_NOACTIVATE) && !ex.contains(WS_EX_TOOLWINDOW));
        assert!(!style.contains(WS_POPUP));
        assert!(PROBE_SHOW.contains(SWP_NOACTIVATE) && PROBE_SHOW.contains(SWP_SHOWWINDOW));
    }

    fn opts(title: &str, activate: bool, topmost: bool) -> TestWindowOptions {
        TestWindowOptions {
            title: title.into(),
            x: 40,
            y: 40,
            client_w: 0,
            client_h: 0,
            activate,
            topmost,
            cell_w: 12,
            cell_h: 20,
            echo_session: 0,
        }
    }

    fn foreground() -> isize {
        // SAFETY: GetForegroundWindow has no preconditions.
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize
    }

    /// D9 for the doctor's probe, on a real desktop: a window this test owns
    /// is foreground (so the process HAS foreground rights), the probe is
    /// shown through the doctor's path, and the foreground must not move.
    /// The positive control — an activated window of ours DOES take it —
    /// proves the check can fail in the same run. Ignored by default: it
    /// moves the foreground, so it runs only on a hosted desktop (the
    /// termhost-bench workflow's windows job), never on a desktop in use.
    #[test]
    #[ignore = "moves the foreground: hosted desktops only (termhost-bench.yml windows job)"]
    fn desktop_the_doctors_probe_never_takes_the_foreground() {
        let owner = GdiTestWindow::create(&opts("termhost-probe d9 owner", true, false))
            .expect("the owner window");
        assert_eq!(
            owner.activate(),
            Ok(true),
            "precondition: our window is foreground"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
        let before = foreground();
        assert_eq!(before, owner.handle().0 as isize, "precondition");
        let probe = GdiTestWindow::create(&opts("termhost-probe doctor", false, true))
            .expect("the probe window");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let during = foreground();
        probe.close();
        assert_eq!(
            during, before,
            "the doctor's probe took the foreground (D9)"
        );
        // Positive control: the selftest's own activating show (a plain
        // create, ShowWindow(SW_SHOW)) moves the foreground — so the
        // assertion above could have failed. Should a plain show not move
        // it on some runner, the explicit activation below still proves the
        // check can see a foreground change; the line printed says which.
        let control = GdiTestWindow::create(&opts("termhost-probe d9 control", true, false))
            .expect("the control window");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut moved = foreground();
        if moved == control.handle().0 as isize {
            eprintln!("positive control: a plain activating show moved the foreground");
        } else {
            let _ = control.activate();
            std::thread::sleep(std::time::Duration::from_millis(200));
            moved = foreground();
            eprintln!("positive control: a plain show did not move it; explicit activation did");
        }
        control.close();
        owner.close();
        assert_eq!(
            moved,
            control.handle().0 as isize,
            "the control must take the foreground"
        );
    }
}
